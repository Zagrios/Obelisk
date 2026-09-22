//! Launcher relations and presence. Lists replace membership; presence never creates it.
use crate::EpicError;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MAX_FRIENDS: usize = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presence {
    #[default]
    Unknown,
    Offline,
    Online,
    Away,
    Busy,
    ExtendedAway,
    Invisible,
}
impl Presence {
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("online" | "available" | "chat") => Self::Online,
            Some("offline" | "unavailable") => Self::Offline,
            Some("away") => Self::Away,
            Some("xa") => Self::ExtendedAway,
            Some("dnd" | "busy") => Self::Busy,
            Some("invisible" | "hidden") => Self::Invisible,
            _ => Self::Unknown,
        }
    }
    fn visible(self) -> bool {
        matches!(
            self,
            Self::Online | Self::Away | Self::Busy | Self::ExtendedAway
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub id: String,
    pub name: Option<String>,
}
impl Activity {
    /// Epic's payload bounds for an activity this account may publish.
    pub(crate) const ID_MAX: usize = 1024;
    pub(crate) const NAME_MAX: usize = 4096;
    pub(crate) fn publishable(&self) -> bool {
        publishable_text(&self.id, Self::ID_MAX)
            && self
                .name
                .as_deref()
                .is_none_or(|name| publishable_text(name, Self::NAME_MAX))
    }
}
/// Shared by the launch catalog reader and the presence API guard, so a bound
/// can never be tightened on one side only.
pub(crate) fn publishable_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Friend {
    pub account_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: Presence,
    pub activity: Option<Activity>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Loading,
    Ready,
    Interrupted,
    Failed,
    Forbidden,
    AuthenticationRequired,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub account_id: String,
    pub revision: u64,
    pub connection: Connection,
    pub friends: Vec<Friend>,
    pub message: Option<String>,
}
impl Snapshot {
    pub(crate) fn new(account_id: String) -> Self {
        Self {
            account_id,
            revision: 1,
            connection: Connection::Loading,
            friends: vec![],
            message: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Roster {
    account_id: String,
    entries: BTreeMap<String, Friend>,
    tombstones: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Relation {
    account_id: String,
    display_name: Option<String>,
    #[serde(default)]
    connections: BTreeMap<String, ExternalConnection>,
}
#[derive(Deserialize)]
struct ExternalConnection {
    avatar: Option<String>,
}
#[derive(Deserialize)]
struct Summary {
    friends: Vec<Relation>,
    #[serde(default)]
    blocklist: Vec<Blocked>,
}
#[derive(Deserialize)]
struct Blocked {
    #[serde(rename = "accountId")]
    account_id: String,
}
pub(crate) fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
        .map(str::to_owned)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
}
impl Roster {
    pub(crate) fn new(account_id: String) -> Self {
        Self {
            account_id,
            ..Self::default()
        }
    }
    pub(crate) fn friends(&self) -> Vec<Friend> {
        self.entries.values().cloned().collect()
    }
    pub(crate) fn apply_profile_names(&mut self, names: &BTreeMap<String, Option<String>>) {
        for (id, friend) in &mut self.entries {
            if friend.display_name.is_none() {
                friend.display_name = names.get(id).cloned().flatten();
            }
        }
    }
    pub(crate) fn retain_tombstones(&mut self, previous: &Self) {
        self.tombstones = previous.tombstones.clone();
        self.entries.retain(|id, _| !self.tombstones.contains(id));
    }

    pub(crate) fn retain_live_presence(&mut self, previous: &Self) {
        // Membership reads contain no presence. Preserve events observed on this
        // same connection for retained friends, never their old metadata.
        for (id, friend) in &mut self.entries {
            if let Some(old) = previous.entries.get(id) {
                friend.presence = old.presence;
                friend.activity = old.activity.clone();
            }
        }
    }

    pub(crate) fn replace_summary(&mut self, value: Value) -> Result<(), EpicError> {
        // This endpoint is a complete summary, with no pagination in either reference.
        // Reject a future paginated schema instead of silently presenting a truncated list.
        if value
            .get("nextCursor")
            .is_some_and(|v| !v.is_null() && v != "")
            || value
                .pointer("/responseMetadata/nextCursor")
                .is_some_and(|v| !v.is_null() && v != "")
        {
            return Err(EpicError::Protocol);
        }
        let summary: Summary = serde_json::from_value(value).map_err(|_| EpicError::Protocol)?;
        if summary.friends.len() > MAX_FRIENDS || summary.blocklist.len() > MAX_FRIENDS {
            return Err(EpicError::Protocol);
        }
        let blocked: BTreeSet<_> = summary
            .blocklist
            .into_iter()
            .map(|v| v.account_id)
            .collect();
        let mut entries: BTreeMap<String, Friend> = BTreeMap::new();
        for relation in summary.friends {
            if !valid_id(&relation.account_id) {
                return Err(EpicError::Protocol);
            }
            if relation.account_id == self.account_id || blocked.contains(&relation.account_id) {
                continue;
            }
            let name = relation
                .display_name
                .filter(|v| !v.trim().is_empty() && v.len() <= 4096);
            let avatar = relation
                .connections
                .into_values()
                .filter_map(|v| v.avatar.and_then(crate::auth::safe_https_url))
                .next();
            if let Some(existing) = entries.get_mut(&relation.account_id) {
                if existing.display_name.is_none() {
                    existing.display_name = name;
                }
                if existing.avatar_url.is_none() {
                    existing.avatar_url = avatar;
                }
            } else {
                entries.insert(
                    relation.account_id.clone(),
                    Friend {
                        account_id: relation.account_id,
                        display_name: name,
                        avatar_url: avatar,
                        presence: Presence::Unknown,
                        activity: None,
                    },
                );
            }
        }
        self.entries = entries;
        Ok(())
    }
    pub(crate) fn presence(&mut self, value: Value) -> Result<(), EpicError> {
        let id = value
            .get("accountId")
            .and_then(Value::as_str)
            .ok_or(EpicError::Protocol)?;
        let Some(friend) = self.entries.get_mut(id) else {
            return Ok(());
        };
        if let Some(status) = value.get("status") {
            friend.presence = Presence::parse(status);
        }
        if !friend.presence.visible() {
            friend.activity = None;
            return Ok(());
        }
        if let Some(namespaces) = value.get("perNs") {
            friend.activity = activity(namespaces)?;
        }
        Ok(())
    }
    /// Returns whether relationship metadata needs an event-triggered refresh.
    pub(crate) fn event(&mut self, event: &Value) -> Result<bool, EpicError> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or(EpicError::Protocol)?;
        let payload = event.get("payload").unwrap_or(&Value::Null);
        if kind == "presence.v1.UPDATE" {
            self.presence(payload.clone())?;
            return Ok(false);
        }
        if !kind.starts_with("friends.v1.") {
            return Ok(false);
        }
        // The current official social component renamed the older EGL3 events.
        // Normalize only their relationship meaning; presence never grants friendship.
        let kind = match kind {
            "friends.v1.FRIENDSHIP_REQUEST" => match payload["status"].as_str() {
                Some("ACCEPTED") => "friends.v1.FRIEND_ENTRY_ADDED",
                Some("PENDING") => "friends.v1.FRIEND_INVITE_ENTRY_ADDED",
                _ => return Ok(false),
            },
            "friends.v1.FRIENDSHIP_REMOVE" if payload["reason"] == "DELETED" => {
                "friends.v1.FRIEND_ENTRY_REMOVED"
            }
            "friends.v1.FRIENDSHIP_REMOVE" => "friends.v1.FRIEND_INVITE_ENTRY_REMOVED",
            "friends.v1.FRIENDSHIP_ENTRY_UPDATE" => "friends.v1.FRIEND_ENTRY_UPDATED",
            "friends.v1.USER_BLOCKLIST_UPDATE" if payload["status"] == "BLOCKED" => {
                "friends.v1.BLOCK_LIST_ENTRY_ADDED"
            }
            "friends.v1.USER_BLOCKLIST_UPDATE" => "friends.v1.BLOCK_LIST_ENTRY_REMOVED",
            _ => kind,
        };
        let from = payload.get("from").and_then(Value::as_str);
        let to = payload.get("to").and_then(Value::as_str);
        let id = if from == Some(&self.account_id) {
            to
        } else if to == Some(&self.account_id) {
            from
        } else {
            payload
                .get("friendId")
                .or_else(|| payload.get("accountId"))
                .and_then(Value::as_str)
        };
        match kind {
            "friends.v1.FRIEND_ENTRY_REMOVED" | "friends.v1.BLOCK_LIST_ENTRY_ADDED" => {
                if let Some(id) = id.filter(|id| valid_id(id)) {
                    if self.tombstones.len() >= MAX_FRIENDS {
                        return Err(EpicError::Protocol);
                    }
                    self.tombstones.insert(id.to_owned());
                    self.entries.remove(id);
                } else {
                    return Err(EpicError::Protocol);
                }
                Ok(true)
            }
            "friends.v1.FRIEND_ENTRY_ADDED" => {
                let id = id.filter(|id| valid_id(id)).ok_or(EpicError::Protocol)?;
                self.tombstones.remove(id);
                if id != self.account_id && !self.entries.contains_key(id) {
                    if self.entries.len() >= MAX_FRIENDS {
                        return Err(EpicError::Protocol);
                    }
                    self.entries.insert(
                        id.into(),
                        Friend {
                            account_id: id.into(),
                            display_name: None,
                            avatar_url: None,
                            presence: Presence::Unknown,
                            activity: None,
                        },
                    );
                }
                Ok(true)
            }
            "friends.v1.FRIEND_ENTRY_UPDATED"
            | "friends.v1.FRIEND_INVITE_ENTRY_ADDED"
            | "friends.v1.FRIEND_INVITE_ENTRY_REMOVED"
            | "friends.v1.BLOCK_LIST_ENTRY_REMOVED" => Ok(true),
            _ => Ok(false),
        }
    }
}

fn activity(value: &Value) -> Result<Option<Activity>, EpicError> {
    if value.is_null() {
        return Ok(None);
    }
    let namespaces = value.as_array().ok_or(EpicError::Protocol)?;
    if namespaces.len() > 128 {
        return Err(EpicError::Protocol);
    }
    let mut candidates = BTreeMap::new();
    for ns in namespaces {
        let namespace = ns
            .get("ns")
            .and_then(Value::as_str)
            .ok_or(EpicError::Protocol)?;
        if !Presence::parse(&ns["status"]).visible() {
            continue;
        }
        let connections = ns
            .get("conns")
            .and_then(Value::as_array)
            .ok_or(EpicError::Protocol)?;
        if connections.len() > 128 {
            return Err(EpicError::Protocol);
        }
        if connections.is_empty() {
            continue;
        }
        for conn in connections {
            let running = conn.get("runningApp");
            let id = running
                .and_then(|v| text(&v["id"]))
                .or_else(|| text(&conn["props"]["running_app_id"]));
            if let Some(id) = id {
                let name = running
                    .and_then(|v| text(&v["displayName"]))
                    .or_else(|| text(&conn["props"]["running_app_name"]));
                candidates.insert(
                    (name.is_none(), namespace.to_owned(), id.clone()),
                    Activity { id, name },
                );
            }
        }
        if !matches!(namespace, "_" | "launcher" | "EGL3") {
            let id = text(&ns["productId"])
                .or_else(|| text(&ns["appId"]))
                .unwrap_or_else(|| namespace.into());
            let name = text(&ns["props"]["EOS_ProductName"])
                .or_else(|| text(&ns["props"]["X_NonEOS_ProductName"]));
            candidates.insert(
                (name.is_none(), namespace.to_owned(), id.clone()),
                Activity { id, name },
            );
        }
    }
    Ok(candidates.into_values().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_filters_relations_and_presence_cannot_create_a_friend() {
        let mut state = Roster::new("owner".into());
        state.replace_summary(json!({"friends":[{"accountId":"a","displayName":"Alice"},{"accountId":"a"},{"accountId":"owner"},{"accountId":"blocked"}],"blocklist":[{"accountId":"blocked"}],"incoming":[{"accountId":"pending"}]})).unwrap();
        state
            .presence(json!({"accountId":"pending","status":"online","perNs":[]}))
            .unwrap();
        assert_eq!(state.friends().len(), 1);
        assert_eq!(state.friends()[0].presence, Presence::Unknown);
        state
            .replace_summary(json!({"friends":[],"blocklist":[]}))
            .unwrap();
        assert!(state.friends().is_empty());
    }
    fn roster() -> Roster {
        let mut r = Roster::new("owner".into());
        r.replace_summary(json!({"friends":[{"accountId":"a","displayName":"Alice"}]}))
            .unwrap();
        r
    }
    fn game(name: &str) -> Value {
        json!({"ns":"_","status":"online","conns":[{"id":"one","runningApp":{"id":name,"displayName":name}},{"id":"two","props":{}}]})
    }
    #[test]
    fn games_change_clear_and_do_not_return_from_stale_hidden_state() {
        let mut r = roster();
        for name in ["Game A", "Game B"] {
            r.presence(json!({"accountId":"a","status":"away","perNs":[game(name)]}))
                .unwrap();
            assert_eq!(
                r.friends()[0].activity.as_ref().unwrap().name.as_deref(),
                Some(name)
            );
            assert_eq!(r.friends()[0].presence, Presence::Away);
        }
        r.presence(json!({"accountId":"a","perNs":[{"ns":"_","status":"online","conns":[{"id":"two","props":{}}]}]})).unwrap();
        assert_eq!(r.friends()[0].presence, Presence::Away);
        assert!(r.friends()[0].activity.is_none());
        r.presence(json!({"accountId":"a","perNs":[game("Again")]}))
            .unwrap();
        r.presence(json!({"accountId":"a","status":"hidden"}))
            .unwrap();
        assert!(r.friends()[0].activity.is_none());
        r.presence(json!({"accountId":"a","status":"online"}))
            .unwrap();
        assert!(r.friends()[0].activity.is_none());
        r.presence(json!({"accountId":"a","perNs":[game("Fresh")]}))
            .unwrap();
        r.presence(json!({"accountId":"a","status":"offline","perNs":[game("Stale")]}))
            .unwrap();
        assert!(r.friends()[0].activity.is_none());
    }
    #[test]
    fn missing_null_and_unknown_fields_have_distinct_effects() {
        let mut r = roster();
        r.presence(json!({"accountId":"a","status":"online","perNs":[game("A")]}))
            .unwrap();
        r.presence(json!({"accountId":"a"})).unwrap();
        assert!(r.friends()[0].activity.is_some());
        r.presence(json!({"accountId":"a","perNs":null})).unwrap();
        assert_eq!(r.friends()[0].presence, Presence::Online);
        assert!(r.friends()[0].activity.is_none());
        r.presence(json!({"accountId":"a","status":"new-server-status","perNs":[game("Hidden")]}))
            .unwrap();
        assert_eq!(r.friends()[0].presence, Presence::Unknown);
        assert!(r.friends()[0].activity.is_none());
        r.presence(json!({"accountId":"a","status":null})).unwrap();
        assert_eq!(r.friends()[0].presence, Presence::Unknown);
    }
    #[test]
    fn namespaces_are_replaced_and_aggregated_independent_of_array_order() {
        let mut r = roster();
        let known = json!({"ns":"eos-game","productId":"product","status":"xa","conns":[{"id":"1"},{"id":"2"}],"props":{"EOS_ProductName":"Known game"}});
        let unknown = json!({"ns":"unknown-product","status":"online","conns":[{"id":"3"}]});
        r.presence(json!({"accountId":"a","status":"xa","perNs":[unknown,known]}))
            .unwrap();
        let expected = r.friends()[0].clone();
        assert_eq!(
            expected.activity.as_ref().unwrap().name.as_deref(),
            Some("Known game")
        );
        r.presence(json!({"accountId":"a","status":"xa","perNs":[known,unknown]}))
            .unwrap();
        assert_eq!(r.friends()[0], expected);
        r.presence(json!({"accountId":"a","perNs":[unknown]}))
            .unwrap();
        assert_eq!(r.friends()[0].activity.as_ref().unwrap().name, None);
        r.presence(json!({"accountId":"a","perNs":[]})).unwrap();
        assert!(r.friends()[0].activity.is_none());
    }
    #[test]
    fn removals_and_blocks_survive_later_metadata_snapshots_until_explicit_acceptance() {
        let mut previous = roster();
        previous.event(&json!({"type":"friends.v1.FRIEND_ENTRY_REMOVED","payload":{"from":"a","to":"owner"}})).unwrap();
        let mut late = roster();
        late.retain_tombstones(&previous);
        late.presence(json!({"accountId":"a","status":"online","perNs":[game("Old")]}))
            .unwrap();
        assert!(late.friends().is_empty());
        late.event(
            &json!({"type":"friends.v1.FRIEND_ENTRY_ADDED","payload":{"from":"owner","to":"a"}}),
        )
        .unwrap();
        assert_eq!(late.friends()[0].presence, Presence::Unknown);
        assert_eq!(late.friends()[0].display_name, None);
        late.event(
            &json!({"type":"friends.v1.BLOCK_LIST_ENTRY_ADDED","payload":{"accountId":"a"}}),
        )
        .unwrap();
        assert!(late.friends().is_empty());
        late.event(&json!({"type":"friends.v1.FRIEND_INVITE_ENTRY_ADDED","payload":{"from":"owner","to":"pending"}})).unwrap();
        assert!(late.friends().is_empty());
    }
    #[test]
    fn current_launcher_relationship_events_preserve_acceptance_and_removal_boundaries() {
        let mut r = roster();
        assert!(r.event(&json!({"type":"friends.v1.FRIENDSHIP_REQUEST","payload":{"from":"owner","to":"b","status":"PENDING"}})).unwrap());
        assert_eq!(r.friends().len(), 1);
        assert!(r.event(&json!({"type":"friends.v1.FRIENDSHIP_REQUEST","payload":{"from":"owner","to":"b","status":"ACCEPTED"}})).unwrap());
        assert_eq!(r.friends().len(), 2);
        assert!(
            r.event(
                &json!({"type":"friends.v1.FRIENDSHIP_ENTRY_UPDATE","payload":{"friendId":"b"}})
            )
            .unwrap()
        );
        assert!(r.event(&json!({"type":"friends.v1.FRIENDSHIP_REMOVE","payload":{"from":"owner","to":"b","reason":"DELETED"}})).unwrap());
        r.event(
            &json!({"type":"presence.v1.UPDATE","payload":{"accountId":"b","status":"online"}}),
        )
        .unwrap();
        assert_eq!(r.friends().len(), 1);
        assert!(r.event(&json!({"type":"friends.v1.USER_BLOCKLIST_UPDATE","payload":{"accountId":"a","status":"BLOCKED"}})).unwrap());
        assert!(r.friends().is_empty());
        assert!(r.event(&json!({"type":"friends.v1.USER_BLOCKLIST_UPDATE","payload":{"accountId":"a","status":"UNBLOCKED"}})).unwrap());
        assert!(r.friends().is_empty());
        let mut stale = roster();
        stale.retain_tombstones(&r);
        assert!(stale.friends().is_empty());
    }

    #[test]
    fn incomplete_summaries_are_rejected_and_unreported_presence_stays_unknown() {
        let mut r = roster();
        assert_eq!(r.friends()[0].presence, Presence::Unknown);
        assert!(
            r.replace_summary(json!({"friends":[],"nextCursor":"unhandled-page"}))
                .is_err()
        );
        assert!(
            r.replace_summary(json!({"friends":[{"accountId":""}]}))
                .is_err()
        );
    }
    #[test]
    fn metadata_uses_only_returned_safe_avatars_and_explicit_snapshot_names() {
        let mut r = roster();
        r.replace_summary(json!({"friends":[{"accountId":"a","displayName":null,"connections":{"steam":{"avatar":"http://unsafe/image"}}}]})).unwrap();
        assert_eq!(r.friends()[0].display_name, None);
        assert_eq!(r.friends()[0].avatar_url, None);
        r.replace_summary(json!({"friends":[{"accountId":"a","connections":{"steam":{"avatar":"https://example.invalid/returned.png"}}}]})).unwrap();
        assert_eq!(
            r.friends()[0].avatar_url.as_deref(),
            Some("https://example.invalid/returned.png")
        );
    }
}
