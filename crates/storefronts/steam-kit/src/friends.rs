//! Account-scoped, event-driven Steam friends state. The watch channel contains
//! complete snapshots, so a slow consumer can coalesce updates without losing
//! relationship removals or field clears. Content requests never wait for it.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError},
};

use prost::Message;
use tokio::sync::watch;

use crate::proto;

pub(crate) const PERSONA_STATE: u32 = 766;
pub(crate) const FRIENDS_LIST: u32 = 767;
pub(crate) const STATUS: u32 = 1;
pub(crate) const PLAYER_NAME: u32 = 2;
pub(crate) const PRESENCE: u32 = 16;
pub(crate) const GAME_EXTRA_INFO: u32 = 256;
pub(crate) const GAME_DATA_BLOB: u32 = 512;
pub(crate) const REQUESTED: u32 = PLAYER_NAME | PRESENCE;
pub(crate) const REQUEST_BATCH: usize = 100;
// These are defensive client limits, not claims about Steam's account limits.
const MAX_FRIENDS: usize = 10_000;
const MAX_EARLY_PERSONAS: usize = 512;
const MAX_SOCIAL_PACKET: usize = 4 * 1024 * 1024;
const MAX_LABEL_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendPresence {
    Unknown,
    Offline,
    Online,
    Busy,
    Away,
    Snooze,
    LookingToTrade,
    LookingToPlay,
    Invisible,
}

impl From<u32> for FriendPresence {
    fn from(value: u32) -> Self {
        match value {
            0 => Self::Offline,
            1 => Self::Online,
            2 => Self::Busy,
            3 => Self::Away,
            4 => Self::Snooze,
            5 => Self::LookingToTrade,
            6 => Self::LookingToPlay,
            7 => Self::Invisible,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendGame {
    pub app_id: Option<u32>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Friend {
    pub steam_id: u64,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: FriendPresence,
    pub game: Option<FriendGame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendsConnection {
    Loading,
    Ready,
    Disconnected,
    AuthenticationRequired,
    Error,
}

impl FriendsConnection {
    /// Terminal states: the reducer is done accepting packets and metadata.
    pub(crate) fn ended(self) -> bool {
        matches!(self, Self::Disconnected | Self::AuthenticationRequired)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendsSnapshot {
    pub revision: u64,
    pub account_id: u64,
    pub connection: FriendsConnection,
    pub friends: Vec<Friend>,
}

#[derive(Clone)]
struct CachedFriend {
    friend: Friend,
    /// Persona flag groups received so far; readiness needs the name, avatar
    /// and status groups, so a status-less identity reply never marks a friend ready.
    received: u32,
}

impl CachedFriend {
    fn new(steam_id: u64) -> Self {
        Self {
            friend: Friend {
                steam_id,
                display_name: None,
                avatar_url: None,
                presence: FriendPresence::Unknown,
                game: None,
            },
            received: 0,
        }
    }

    fn apply(&mut self, flags: u32, persona: &proto::Persona) {
        self.received |= flags;
        if flags & PLAYER_NAME != 0 {
            self.friend.display_name = persona.player_name.as_deref().and_then(label);
        }
        if flags & PRESENCE != 0
            && let Some(hash) = &persona.avatar_hash
        {
            self.friend.avatar_url = crate::cm::avatar_url_from_hash(hash);
        }
        // Valve's shipped friends client treats Status1 as the complete
        // presence/activity group, with protobuf defaults for omitted scalars.
        // Identity-only responses (mask18) must never erase this group.
        if flags & STATUS != 0 {
            self.friend.presence = persona.persona_state.unwrap_or(0).into();
            let app_id = persona
                .game_played_app_id
                .filter(|id| *id != 0)
                .or_else(|| {
                    persona
                        .game_id
                        .map(|id| (id & 0x00ff_ffff) as u32)
                        .filter(|id| *id != 0)
                });
            self.friend.game = (app_id.is_some() || persona.game_id.is_some_and(|id| id != 0))
                .then(|| FriendGame {
                    app_id,
                    name: persona.game_name.as_deref().and_then(label),
                });
        } else {
            // Compatibility with explicitly populated historical game groups.
            // Missing groups/fields here remain partial updates.
            if flags & GAME_DATA_BLOB != 0
                && let Some(app_id) = persona.game_played_app_id
            {
                if app_id == 0 {
                    self.friend.game = None;
                } else {
                    let game = self.friend.game.get_or_insert(FriendGame {
                        app_id: None,
                        name: None,
                    });
                    if game.app_id != Some(app_id) {
                        game.name = None;
                    }
                    game.app_id = Some(app_id);
                }
            }
            if flags & (GAME_EXTRA_INFO | GAME_DATA_BLOB) != 0 {
                if let Some(game_id) = persona.game_id {
                    if game_id == 0 {
                        self.friend.game = None;
                    } else {
                        // CGameID's low 24 bits are the app ID. Non-Steam shortcuts
                        // can have no app ID while still supplying a game name.
                        let app_id = (game_id & 0x00ff_ffff) as u32;
                        let game = self.friend.game.get_or_insert(FriendGame {
                            app_id: None,
                            name: None,
                        });
                        let app_id = (app_id != 0).then_some(app_id);
                        if game.app_id != app_id {
                            game.name = None;
                        }
                        game.app_id = app_id;
                    }
                }
                if let Some(name) = &persona.game_name {
                    let name = label(name);
                    if let Some(game) = &mut self.friend.game {
                        game.name = name;
                    } else if name.is_some()
                        && persona.game_id != Some(0)
                        && (persona.game_played_app_id != Some(0)
                            || persona.game_id.is_some_and(|id| id != 0))
                    {
                        self.friend.game = Some(FriendGame { app_id: None, name });
                    }
                }
            }
        }
        // An explicit offline event invalidates activity even without a game
        // field. Later online-only events cannot resurrect the old game.
        if matches!(
            self.friend.presence,
            FriendPresence::Offline | FriendPresence::Invisible
        ) {
            self.friend.game = None;
        }
    }
}

fn label(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut end = value.len().min(MAX_LABEL_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

fn individual(id: u64, account_id: u64) -> bool {
    id != account_id && id as u32 != 0 && (id >> 52) & 0xf == 1
}

// Count repeated top-level friends before prost allocates their structs. Tiny
// empty nested messages otherwise amplify a bounded frame into a huge Vec.
fn within_entry_limit(mut body: &[u8]) -> bool {
    let mut count = 0;
    while !body.is_empty() {
        let Ok((tag, wire)) = prost::encoding::decode_key(&mut body) else {
            return true; // Normal decoding handles malformed optional packets.
        };
        if tag == 2 {
            count += 1;
            if count > MAX_FRIENDS {
                return false;
            }
        }
        if prost::encoding::skip_field(
            wire,
            tag,
            &mut body,
            prost::encoding::DecodeContext::default(),
        )
        .is_err()
        {
            return true;
        }
    }
    true
}

struct Reducer {
    account_id: u64,
    full_list_received: bool,
    relationships_valid: bool,
    connection: FriendsConnection,
    friends: BTreeMap<u64, CachedFriend>,
    // Only before the first full list: afterwards unknown persona events are
    // ignored, so late updates cannot recreate a removed relationship.
    early: BTreeMap<u64, CachedFriend>,
    // Session-local PICS names, including unknown apps to avoid repeated queries.
    game_names: BTreeMap<u32, Option<String>>,
    dirty: bool,
}

impl Reducer {
    fn new(account_id: u64) -> Self {
        Self {
            account_id,
            full_list_received: false,
            relationships_valid: false,
            connection: FriendsConnection::Loading,
            friends: BTreeMap::new(),
            early: BTreeMap::new(),
            game_names: BTreeMap::new(),
            dirty: false,
        }
    }

    /// An oversized or undecodable social packet invalidates relationships
    /// until the next full list; nothing further is hydrated from it.
    fn fail(&mut self) -> Vec<u64> {
        self.connection = FriendsConnection::Error;
        self.relationships_valid = false;
        self.dirty = true;
        vec![]
    }

    fn relationships(&mut self, list: proto::FriendsList) -> Vec<u64> {
        if list.friends.len() > MAX_FRIENDS {
            return self.fail();
        }
        let accepted: BTreeMap<_, _> = list
            .friends
            .into_iter()
            .filter(|item| individual(item.steam_id, self.account_id))
            .map(|item| (item.steam_id, item.relationship == 3))
            .collect();
        if list.incremental
            && self.friends.len()
                + accepted
                    .iter()
                    .filter(|(id, yes)| **yes && !self.friends.contains_key(id))
                    .count()
                > MAX_FRIENDS
        {
            return self.fail();
        }
        if !list.incremental {
            self.friends.retain(|id, _| accepted.get(id) == Some(&true));
            self.full_list_received = true;
            self.relationships_valid = true;
        }
        let mut hydrate = Vec::new();
        for (id, accepted) in accepted {
            if accepted {
                let newly_added = !self.friends.contains_key(&id);
                self.friends.entry(id).or_insert_with(|| {
                    self.early
                        .remove(&id)
                        .unwrap_or_else(|| CachedFriend::new(id))
                });
                if !list.incremental || newly_added {
                    // Every full list rehydrates everyone, including offline
                    // friends, and every accepted relationship gets fresh data.
                    hydrate.push(id);
                }
            } else {
                self.friends.remove(&id);
                self.early.remove(&id);
            }
        }
        if self.full_list_received {
            self.early.clear();
        }
        self.update_readiness();
        self.dirty = true;
        hydrate
    }

    fn personas(&mut self, state: proto::PersonaState) {
        if state.friends.len() > MAX_FRIENDS {
            self.connection = FriendsConnection::Error;
            self.dirty = true;
            return;
        }
        for persona in state.friends {
            if !individual(persona.friend_id, self.account_id) {
                continue;
            }
            if let Some(cached) = self.friends.get_mut(&persona.friend_id) {
                let previous = cached.friend.clone();
                cached.apply(state.status_flags, &persona);
                self.dirty |= previous != cached.friend;
            } else if !self.full_list_received {
                if self.early.len() >= MAX_EARLY_PERSONAS
                    && !self.early.contains_key(&persona.friend_id)
                {
                    // The full list subsequently requests every real friend;
                    // evicting speculative pre-list cache cannot lose state.
                    self.early.pop_first();
                }
                self.early
                    .entry(persona.friend_id)
                    .or_insert_with(|| CachedFriend::new(persona.friend_id))
                    .apply(state.status_flags, &persona);
            }
        }
        self.update_readiness();
    }

    fn update_readiness(&mut self) {
        let ready = self.full_list_received
            && self.relationships_valid
            && self.friends.values().all(|cached| {
                cached.received & (PLAYER_NAME | PRESENCE | STATUS)
                    == PLAYER_NAME | PRESENCE | STATUS
            });
        if ready
            && matches!(
                self.connection,
                FriendsConnection::Loading | FriendsConnection::Error
            )
        {
            self.connection = FriendsConnection::Ready;
            self.dirty = true;
        }
    }
}

/// The single CM reader reduces packets; optional PICS metadata fills game titles.
/// Public mutation methods let debug fixtures exercise the same observation path.
/// Read/await methods touch only a watch receiver, never the reducer mutex.
#[derive(Clone)]
pub struct FriendsEventSource {
    reducer: Arc<Mutex<Reducer>>,
    updates: watch::Sender<Arc<FriendsSnapshot>>,
}

impl FriendsEventSource {
    pub fn new(account_id: u64) -> Self {
        let (updates, _) = watch::channel(Arc::new(FriendsSnapshot {
            revision: 1,
            account_id,
            connection: FriendsConnection::Loading,
            friends: vec![],
        }));
        Self {
            reducer: Arc::new(Mutex::new(Reducer::new(account_id))),
            updates,
        }
    }

    pub(crate) fn set_account(&self, account_id: u64) {
        *self.reducer.lock().unwrap_or_else(PoisonError::into_inner) = Reducer::new(account_id);
        self.updates.send_modify(|snapshot| {
            *snapshot = Arc::new(FriendsSnapshot {
                revision: snapshot.revision + 1,
                account_id,
                connection: FriendsConnection::Loading,
                friends: vec![],
            });
        });
    }

    pub fn snapshot(&self) -> FriendsSnapshot {
        // Release the watch's read guard before cloning the friend list.
        let snapshot = self.updates.borrow().clone();
        (*snapshot).clone()
    }

    /// The account alone, without cloning the friend list `snapshot` copies.
    pub(crate) fn account_id(&self) -> u64 {
        self.updates.borrow().account_id
    }

    /// Atomic revision check + subscription. Dropping the future cancels the
    /// wait; disconnect wakes existing waits even when no persona changed.
    pub async fn next(&self, after_revision: u64) -> FriendsSnapshot {
        let ready = sailover_core::watch::next_matching(self.updates.subscribe(), |snapshot| {
            snapshot.revision != after_revision || snapshot.connection.ended()
        })
        .await;
        match ready {
            Ok(snapshot) => (*snapshot).clone(),
            Err(snapshot) => {
                let mut snapshot = (*snapshot).clone();
                snapshot.connection = FriendsConnection::Disconnected;
                snapshot
            }
        }
    }

    /// Returns IDs requiring a bounded ClientRequestFriendData hydration.
    /// Unknown or malformed optional social packets do not fail CM queries.
    pub fn apply_packet(&self, kind: u32, body: &[u8]) -> Vec<u64> {
        let hydrate = self.apply_packet_deferred(kind, body);
        self.publish();
        hydrate
    }

    pub(crate) fn apply_packet_deferred(&self, kind: u32, body: &[u8]) -> Vec<u64> {
        if !matches!(kind, FRIENDS_LIST | PERSONA_STATE) {
            return vec![];
        }
        let mut reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if reducer.connection.ended() {
            return vec![];
        }
        if body.len() > MAX_SOCIAL_PACKET || !within_entry_limit(body) {
            return reducer.fail();
        }
        match kind {
            FRIENDS_LIST => match proto::FriendsList::decode(body) {
                Ok(list) => reducer.relationships(list),
                Err(_) => reducer.fail(),
            },
            PERSONA_STATE => {
                if let Ok(state) = proto::PersonaState::decode(body) {
                    reducer.personas(state);
                }
                vec![]
            }
            _ => vec![],
        }
    }

    pub(crate) fn publish(&self) {
        let mut reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if !reducer.dirty {
            return;
        }
        reducer.dirty = false;
        let friends: Vec<_> = reducer
            .friends
            .values()
            .map(|cached| {
                let mut friend = cached.friend.clone();
                if let Some(game) = &mut friend.game
                    && game.name.is_none()
                    && let Some(app_id) = game.app_id
                {
                    game.name = reducer.game_names.get(&app_id).cloned().flatten();
                }
                friend
            })
            .collect();
        self.updates.send_if_modified(|snapshot| {
            if snapshot.account_id == reducer.account_id
                && snapshot.connection == reducer.connection
                && snapshot.friends == friends
            {
                return false;
            }
            *snapshot = Arc::new(FriendsSnapshot {
                revision: snapshot.revision + 1,
                account_id: reducer.account_id,
                connection: reducer.connection,
                friends,
            });
            true
        });
    }

    pub fn disconnect(&self, authentication_required: bool) {
        let mut reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if reducer.connection != FriendsConnection::AuthenticationRequired {
            reducer.connection = if authentication_required {
                FriendsConnection::AuthenticationRequired
            } else {
                FriendsConnection::Disconnected
            };
        }
        reducer.dirty = true;
        drop(reducer);
        self.publish();
    }

    pub(crate) fn loading_timed_out(&self) {
        let mut reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if reducer.connection == FriendsConnection::Loading {
            reducer.connection = FriendsConnection::Error;
            reducer.dirty = true;
        }
        drop(reducer);
        self.publish();
    }

    pub(crate) fn retains(&self, id: u64) -> bool {
        self.reducer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .friends
            .contains_key(&id)
    }

    /// Distinct apps whose title is still unknown. Bounded by the friend cap;
    /// the caller batches, since PICS request size is a transport concern.
    pub(crate) fn missing_game_names(&self) -> Vec<u32> {
        let reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if reducer.connection.ended() {
            return vec![];
        }
        reducer
            .friends
            .values()
            .filter_map(|cached| cached.friend.game.as_ref())
            .filter(|game| game.name.is_none())
            .filter_map(|game| game.app_id)
            .filter(|id| !reducer.game_names.contains_key(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn resolve_game_names(&self, account_id: u64, names: &[(u32, Option<String>)]) {
        let mut reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        if reducer.account_id != account_id || reducer.connection.ended() {
            return;
        }
        for (id, name) in names {
            // An unresolved app caches as None so it is never queried twice; that
            // publishes nothing, so only a changed title needs a snapshot rebuild.
            let resolved = name.as_deref().and_then(label);
            reducer.dirty |=
                reducer.game_names.get(id).and_then(Option::as_deref) != resolved.as_deref();
            reducer.game_names.insert(*id, resolved);
        }
        drop(reducer);
        self.publish();
    }

    pub(crate) fn has_full_list(&self) -> bool {
        self.reducer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .full_list_received
    }

    pub(crate) fn retain_hydration(&self, pending: &mut BTreeSet<u64>) {
        let reducer = self.reducer.lock().unwrap_or_else(PoisonError::into_inner);
        pending.retain(|id| reducer.friends.contains_key(id));
    }
}

/// What a CM session still owes the friends list: one push of every friend's
/// persona state once the full list arrived, and the details of friends it
/// revealed, requested in bounded batches. The connection sends the messages.
#[derive(Default)]
pub(crate) struct Hydration {
    pending: BTreeSet<u64>,
    push_pending: bool,
    push_in_flight: bool,
    deadline: Option<tokio::time::Instant>,
}

impl Hydration {
    /// A session started; its friends list must load in time.
    pub(crate) fn started(&mut self) {
        self.deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(20));
    }

    pub(crate) fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    pub(crate) fn timed_out(&mut self, friends: &FriendsEventSource) {
        friends.loading_timed_out();
        self.deadline = None;
    }

    /// Applies one packet, noting the friends whose details it revealed.
    pub(crate) fn apply(&mut self, friends: &FriendsEventSource, kind: u32, body: &[u8]) {
        let revealed = friends.apply_packet_deferred(kind, body);
        self.push_pending |= !revealed.is_empty();
        self.pending.extend(revealed);
        if kind == FRIENDS_LIST {
            friends.retain_hydration(&mut self.pending);
        }
    }

    /// Whether to ask Steam to push every friend's persona state now. Bursts
    /// of relationship changes coalesce into one push at a time.
    pub(crate) fn push_due(&self, friends: &FriendsEventSource) -> bool {
        self.push_pending && !self.push_in_flight && friends.has_full_list()
    }

    pub(crate) fn push_sent(&mut self) {
        self.push_pending = false;
        self.push_in_flight = true;
    }

    pub(crate) fn push_answered(&mut self) {
        self.push_in_flight = false;
    }

    pub(crate) fn has_batch(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The next friends whose details to request, still in the list.
    pub(crate) fn next_batch(&mut self, friends: &FriendsEventSource) -> Vec<u64> {
        (0..REQUEST_BATCH)
            .filter_map(|_| self.pending.pop_first())
            .filter(|id| friends.retains(*id))
            .collect()
    }
}

/// EResult authentication/session invalidation values from SteamKit's pinned
/// enum. Service outages and transport failures remain reconnectable.
pub fn authentication_required(result: i32) -> bool {
    matches!(
        result,
        5 | 6 | 15 | 18 | 26 | 27 | 34 | 43 | 50 | 63 | 65 | 66 | 71 | 74 | 77 | 85 | 88 | 89 | 93
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    const ACCOUNT: u64 = 76_561_197_960_265_730;
    const FRIEND: u64 = 76_561_197_960_265_729;
    // Hand-authored wire fixtures, checked independently against SteamTracking
    // 08f0f630b3ada21a6a48ffede2116df3ef3c918d, friends.proto fields. No
    // project protobuf encoder produced these bytes. IDs/names are synthetic.
    const FULL_LIST: &[u8] = &[
        0x08, 0x00, 0x12, 0x0b, 0x09, 0x01, 0, 0, 0, 0x01, 0, 0x10, 0x01, 0x10, 0x03,
    ];
    const PERSONA: &[u8] = &[
        0x08, 0x93, 0x06, 0x12, 0x2a, 0x09, 0x01, 0, 0, 0, 0x01, 0, 0x10, 0x01, 0x10, 0x03, 0x18,
        0xba, 0x04, 0x7a, 0x03, 0x42, 0x6f, 0x62, 0xfa, 0x01, 0x03, 0x01, 0x02, 0x03, 0xba, 0x03,
        0x04, 0x47, 0x61, 0x6d, 0x65, 0xc1, 0x03, 0x3a, 0x02, 0, 0, 0, 0, 0, 0,
    ];

    fn list(source: &FriendsEventSource, incremental: bool, entries: &[(u64, u32)]) -> Vec<u64> {
        source.apply_packet(
            FRIENDS_LIST,
            &proto::FriendsList {
                incremental,
                friends: entries
                    .iter()
                    .map(|(steam_id, relationship)| proto::FriendRelationship {
                        steam_id: *steam_id,
                        relationship: *relationship,
                    })
                    .collect(),
            }
            .encode_to_vec(),
        )
    }

    fn persona(source: &FriendsEventSource, flags: u32, update: proto::Persona) {
        source.apply_packet(
            PERSONA_STATE,
            &proto::PersonaState {
                status_flags: flags,
                friends: vec![update],
            }
            .encode_to_vec(),
        );
    }

    fn update() -> proto::Persona {
        proto::Persona {
            friend_id: FRIEND,
            ..Default::default()
        }
    }

    /// Published title of every friend, which each must be in a game to have.
    fn titles(source: &FriendsEventSource) -> Vec<Option<String>> {
        source
            .snapshot()
            .friends
            .iter()
            .map(|friend| friend.game.as_ref().expect("friend in a game").name.clone())
            .collect()
    }

    fn title(source: &FriendsEventSource) -> Option<String> {
        titles(source).remove(0)
    }

    #[test]
    fn resolved_titles_follow_current_activity_and_preserve_persona_names() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3), (FRIEND + 2, 3)]);
        let playing = proto::Persona {
            persona_state: Some(1),
            game_played_app_id: Some(620),
            ..update()
        };
        persona(&source, STATUS, playing.clone());
        persona(
            &source,
            STATUS,
            proto::Persona {
                friend_id: FRIEND + 2,
                ..playing.clone()
            },
        );
        assert_eq!(source.missing_game_names(), [620]);
        source.resolve_game_names(ACCOUNT, &[(620, Some(" Portal 2 ".into()))]);
        let portal = || Some("Portal 2".to_owned());
        assert_eq!(titles(&source), [portal(), portal()]);
        assert!(source.missing_game_names().is_empty());

        // A repeated persona frame keeps the resolved title.
        persona(&source, STATUS, playing.clone());
        assert_eq!(title(&source).as_deref(), Some("Portal 2"));

        persona(
            &source,
            STATUS,
            proto::Persona {
                game_played_app_id: Some(570),
                ..playing
            },
        );
        assert_eq!(title(&source), None);
        assert_eq!(source.missing_game_names(), [570]);
        persona(&source, STATUS, update());
        source.resolve_game_names(ACCOUNT, &[(570, Some("Dota 2".into()))]);
        assert!(
            source.snapshot().friends[0].game.is_none(),
            "Late metadata cannot restore a closed game"
        );
    }

    #[test]
    fn a_persona_supplied_title_wins_over_resolved_metadata() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        let playing = proto::Persona {
            persona_state: Some(1),
            game_played_app_id: Some(620),
            ..update()
        };
        persona(&source, STATUS, playing.clone());
        source.resolve_game_names(ACCOUNT, &[(620, Some("Portal 2".into()))]);
        assert_eq!(title(&source).as_deref(), Some("Portal 2"));
        persona(
            &source,
            STATUS,
            proto::Persona {
                game_name: Some("Custom title".into()),
                ..playing
            },
        );
        assert_eq!(title(&source).as_deref(), Some("Custom title"));
    }

    #[test]
    fn switching_account_drops_resolved_titles_and_rejects_the_old_accounts_answers() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(1),
                game_played_app_id: Some(620),
                ..update()
            },
        );
        source.resolve_game_names(ACCOUNT, &[(620, Some("Portal 2".into()))]);

        source.set_account(ACCOUNT + 1);
        source.resolve_game_names(ACCOUNT, &[(620, Some("Old account result".into()))]);
        assert!(source.reducer.lock().unwrap().game_names.is_empty());
    }

    #[test]
    fn unknown_titles_are_cached_without_publishing_false_activity() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(1),
                game_played_app_id: Some(620),
                ..update()
            },
        );
        let before = source.snapshot();
        source.resolve_game_names(ACCOUNT, &[(620, Some("   ".into()))]);
        assert_eq!(source.snapshot(), before);
        assert!(source.missing_game_names().is_empty());
        source.disconnect(false);
        source.resolve_game_names(ACCOUNT, &[(620, Some("Late title".into()))]);
        assert_eq!(title(&source), None);
    }

    #[test]
    fn independent_wire_fixtures_decode_group_fields_and_initial_offline_hydration() {
        let source = FriendsEventSource::new(ACCOUNT);
        assert_eq!(source.apply_packet(FRIENDS_LIST, FULL_LIST), [FRIEND]);
        assert_eq!(source.snapshot().connection, FriendsConnection::Loading);
        source.apply_packet(PERSONA_STATE, PERSONA);
        let snapshot = source.snapshot();
        assert_eq!(snapshot.connection, FriendsConnection::Ready);
        assert_eq!(snapshot.friends[0].display_name.as_deref(), Some("Bob"));
        assert_eq!(snapshot.friends[0].presence, FriendPresence::Away);
        assert_eq!(
            snapshot.friends[0].game,
            Some(FriendGame {
                app_id: Some(570),
                name: Some("Game".into())
            })
        );
        assert_eq!(snapshot.friends[0].avatar_url, None);
        // Independently encoded presence-only Offline: field2 explicitly zero.
        let offline = [
            0x08, 0x01, 0x12, 0x0b, 0x09, 0x01, 0, 0, 0, 0x01, 0, 0x10, 0x01, 0x10, 0,
        ];
        source.apply_packet(PERSONA_STATE, &offline);
        assert_eq!(
            source.snapshot().friends[0].presence,
            FriendPresence::Offline
        );
        assert_eq!(source.snapshot().friends[0].game, None);
        assert_eq!(list(&source, false, &[(FRIEND, 3)]), [FRIEND]);
    }

    #[test]
    fn live_default_offline_status_group_completes_identity_only_hydration() {
        let source = FriendsEventSource::new(ACCOUNT);
        source.apply_packet(FRIENDS_LIST, FULL_LIST);
        // Independent wire fixture: Status1, friendID only. Valve's reducer
        // defaults missing field2 to Offline inside this authoritative group.
        let status = [
            0x08, 0x01, 0x12, 0x09, 0x09, 0x01, 0, 0, 0, 0x01, 0, 0x10, 0x01,
        ];
        source.apply_packet(PERSONA_STATE, &status);
        // Names/avatars arrive separately, as observed on the normal live CM
        // session. No presence fields or game fields in this identity packet.
        let identity = [
            0x08, 0x12, 0x12, 0x14, 0x09, 0x01, 0, 0, 0, 0x01, 0, 0x10, 0x01, 0x7a, 3, b'B', b'o',
            b'b', 0xfa, 0x01, 3, 1, 2, 3,
        ];
        source.apply_packet(PERSONA_STATE, &identity);
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
        assert_eq!(
            source.snapshot().friends[0].presence,
            FriendPresence::Offline
        );
        source.loading_timed_out();
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
    }

    #[test]
    fn full_and_incremental_relationships_filter_deduplicate_and_remove_without_revival() {
        let source = FriendsEventSource::new(ACCOUNT);
        let clan = (7_u64 << 52) | 1;
        assert_eq!(
            list(
                &source,
                false,
                &[
                    (ACCOUNT, 3),
                    (clan, 3),
                    (FRIEND, 3),
                    (FRIEND, 3),
                    (FRIEND + 2, 2),
                    (FRIEND + 3, 1),
                    (FRIEND + 4, 4),
                    (FRIEND + 5, 999)
                ]
            ),
            [FRIEND]
        );
        assert_eq!(source.snapshot().friends.len(), 1);
        source.apply_packet(PERSONA_STATE, PERSONA);
        list(&source, true, &[(FRIEND, 0), (FRIEND + 2, 3)]);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert_eq!(
            source
                .snapshot()
                .friends
                .iter()
                .map(|f| f.steam_id)
                .collect::<Vec<_>>(),
            [FRIEND + 2]
        );
        list(&source, true, &[(FRIEND + 2, 1)]);
        assert!(source.snapshot().friends.is_empty());
        list(&source, true, &[(FRIEND, 3)]);
        assert_eq!(source.snapshot().friends[0].display_name, None);
        list(&source, false, &[]);
        assert!(source.snapshot().friends.is_empty());
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
    }

    #[test]
    fn personas_before_full_list_are_hydrated_without_losing_partial_groups() {
        let source = FriendsEventSource::new(ACCOUNT);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert!(source.snapshot().friends.is_empty());
        list(&source, false, &[(FRIEND, 3)]);
        assert_eq!(
            source.snapshot().friends[0].display_name.as_deref(),
            Some("Bob")
        );
        let initial = source.snapshot();
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(2),
                game_id: Some(570),
                game_name: Some("Game".into()),
                ..update()
            },
        );
        let busy = source.snapshot();
        assert_eq!(busy.friends[0].presence, FriendPresence::Busy);
        assert_eq!(busy.friends[0].game, initial.friends[0].game);
        assert_eq!(
            busy.friends[0].display_name,
            initial.friends[0].display_name
        );
        persona(&source, PRESENCE, update());
        assert_eq!(source.snapshot().friends, busy.friends);
        persona(
            &source,
            PLAYER_NAME | PRESENCE,
            proto::Persona {
                player_name: Some("New name".into()),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].presence, FriendPresence::Busy);
        assert_eq!(source.snapshot().friends[0].game, busy.friends[0].game);
        // Name2 has a default empty value when its field is omitted, while
        // absence of the entire Status group preserves presence and game.
        persona(&source, PLAYER_NAME, update());
        assert_eq!(source.snapshot().friends[0].display_name, None);
        persona(
            &source,
            PLAYER_NAME | PRESENCE,
            proto::Persona {
                player_name: Some(String::new()),
                avatar_hash: Some(vec![]),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].display_name, None);
        assert_eq!(source.snapshot().friends[0].presence, FriendPresence::Busy);
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(1),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].game, None);
    }

    #[test]
    fn explicit_game_closes_and_offline_clears_survive_later_online_events() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        persona(
            &source,
            REQUESTED | STATUS,
            proto::Persona {
                persona_state: Some(1),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].game, None);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert!(source.snapshot().friends[0].game.is_some());
        persona(
            &source,
            GAME_EXTRA_INFO,
            proto::Persona {
                game_id: Some(0),
                game_name: Some(String::new()),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].game, None);
        source.apply_packet(PERSONA_STATE, PERSONA);
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(0),
                ..update()
            },
        );
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(1),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].game, None);
        source.apply_packet(PERSONA_STATE, PERSONA);
        persona(
            &source,
            STATUS | GAME_EXTRA_INFO,
            proto::Persona {
                game_played_app_id: Some(0),
                game_name: Some("Stale".into()),
                ..update()
            },
        );
        assert_eq!(source.snapshot().friends[0].game, None);
    }

    #[test]
    fn unknown_values_bad_optional_packets_and_missing_names_do_not_crash() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        persona(
            &source,
            REQUESTED | STATUS,
            proto::Persona {
                persona_state: Some(500),
                ..update()
            },
        );
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
        assert_eq!(
            source.snapshot().friends[0].presence,
            FriendPresence::Unknown
        );
        let before = source.snapshot();
        source.apply_packet(PERSONA_STATE, &[0xff]);
        assert_eq!(source.snapshot(), before);
        persona(
            &source,
            STATUS,
            proto::Persona {
                persona_state: Some(7),
                game_played_app_id: Some(570),
                ..update()
            },
        );
        assert_eq!(
            source.snapshot().friends[0].presence,
            FriendPresence::Invisible
        );
        assert_eq!(source.snapshot().friends[0].game, None);
    }

    #[tokio::test]
    async fn observation_is_atomic_coalesces_exact_snapshots_and_can_be_cancelled() {
        let source = FriendsEventSource::new(ACCOUNT);
        let before = source.snapshot().revision;
        list(&source, false, &[(FRIEND, 3)]);
        // The update happened before installing the wait: revision catches it.
        let mut latest = source.next(before).await;
        for state in 0..1000 {
            persona(
                &source,
                STATUS,
                proto::Persona {
                    persona_state: Some(state % 5),
                    ..update()
                },
            );
        }
        latest = source.next(latest.revision).await;
        assert_eq!(latest.friends[0].presence, FriendPresence::Snooze);
        let wait = source.next(latest.revision);
        tokio::pin!(wait);
        std::future::poll_fn(|cx| {
            assert!(matches!(
                std::future::Future::poll(wait.as_mut(), cx),
                Poll::Pending
            ));
            Poll::Ready(())
        })
        .await;
        source.disconnect(false);
        let disconnected = wait.await;
        assert_eq!(disconnected.connection, FriendsConnection::Disconnected);
        assert_eq!(disconnected.friends[0].presence, FriendPresence::Snooze);
        let other = FriendsEventSource::new(ACCOUNT + 1);
        let cancellation = tokio::spawn(async move { other.next(other.snapshot().revision).await });
        cancellation.abort();
        assert!(cancellation.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn repeat_updates_do_not_bump_revision_and_account_change_clears_old_state() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        source.apply_packet(PERSONA_STATE, PERSONA);
        let before = source.snapshot();
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert_eq!(source.snapshot(), before);
        source.set_account(ACCOUNT + 1);
        assert!(source.snapshot().friends.is_empty());
        assert_eq!(source.snapshot().connection, FriendsConnection::Loading);
        assert_eq!(source.snapshot().account_id, ACCOUNT + 1);
    }

    #[test]
    fn loading_timeout_and_authentication_loss_are_distinct_and_keep_last_data() {
        let source = FriendsEventSource::new(ACCOUNT);
        list(&source, false, &[(FRIEND, 3)]);
        source.loading_timed_out();
        assert_eq!(source.snapshot().connection, FriendsConnection::Error);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
        let previous = source.snapshot().friends;
        source.disconnect(true);
        source.disconnect(false);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert_eq!(
            source.snapshot().connection,
            FriendsConnection::AuthenticationRequired
        );
        assert_eq!(source.snapshot().friends, previous);
        assert!(authentication_required(26));
        assert!(authentication_required(77));
        assert!(!authentication_required(3));
        assert!(!authentication_required(48));
    }

    #[test]
    fn speculative_persona_cache_and_relationship_memory_are_bounded() {
        let source = FriendsEventSource::new(ACCOUNT);
        for offset in 10..MAX_EARLY_PERSONAS + 30 {
            persona(
                &source,
                PLAYER_NAME,
                proto::Persona {
                    friend_id: FRIEND + offset as u64,
                    player_name: Some("Name".into()),
                    ..Default::default()
                },
            );
        }
        assert_eq!(
            source.reducer.lock().unwrap().early.len(),
            MAX_EARLY_PERSONAS
        );
        let excessive = vec![(FRIEND, 3); MAX_FRIENDS + 1];
        list(&source, false, &excessive);
        assert_eq!(source.snapshot().connection, FriendsConnection::Error);
        assert!(source.snapshot().friends.is_empty());
        list(&source, false, &[(FRIEND, 3)]);
        assert!(source.reducer.lock().unwrap().early.is_empty());
    }

    #[test]
    fn lost_relationship_delta_stays_stale_until_full_resynchronization() {
        let source = FriendsEventSource::new(ACCOUNT);
        source.apply_packet(FRIENDS_LIST, FULL_LIST);
        source.apply_packet(PERSONA_STATE, PERSONA);
        source.apply_packet(FRIENDS_LIST, &[0xff]);
        source.apply_packet(PERSONA_STATE, PERSONA);
        assert_eq!(source.snapshot().connection, FriendsConnection::Error);
        list(&source, true, &[(FRIEND, 3)]);
        assert_eq!(source.snapshot().connection, FriendsConnection::Error);
        source.apply_packet(FRIENDS_LIST, FULL_LIST);
        assert_eq!(source.snapshot().connection, FriendsConnection::Ready);
        // Reject before allocating MAX_FRIENDS+1 nested Persona structs.
        let oversized = [0x12, 0].repeat(MAX_FRIENDS + 1);
        assert!(!within_entry_limit(&oversized));
        source.apply_packet(PERSONA_STATE, &oversized);
        assert_eq!(source.snapshot().connection, FriendsConnection::Error);
    }
}
