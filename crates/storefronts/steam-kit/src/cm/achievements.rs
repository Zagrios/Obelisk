//! Read-only Player services. Wire fields follow steammessages_player.steamclient.proto.
use super::{CmError, SteamConnection};
use prost::Message;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Achievement {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon_url: String,
    pub hidden: bool,
    pub unlocked: bool,
    pub unlock_time: u32,
}

#[derive(Clone, PartialEq, Message)]
struct GameRequest {
    #[prost(uint32, tag = "1")]
    app_id: u32,
    #[prost(string, tag = "2")]
    language: String,
}
#[derive(Clone, PartialEq, Message)]
struct UserRequest {
    #[prost(uint64, tag = "1")]
    steam_id: u64,
    #[prost(uint32, tag = "2")]
    app_id: u32,
}
#[derive(Clone, PartialEq, Message)]
pub(crate) struct GameResponse {
    #[prost(message, repeated, tag = "1")]
    achievements: Vec<Definition>,
    #[prost(int32, optional, tag = "2")]
    schema_version: Option<i32>,
    #[prost(uint32, optional, tag = "4")]
    schema_hash: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
struct Definition {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(string, tag = "3")]
    description: String,
    #[prost(string, tag = "4")]
    icon: String,
    #[prost(string, tag = "5")]
    icon_gray: String,
    #[prost(bool, tag = "6")]
    hidden: bool,
    #[prost(uint32, optional, tag = "8")]
    key: Option<u32>,
    #[prost(bool, tag = "12")]
    archived: bool,
}
#[derive(Clone, PartialEq, Message)]
pub(crate) struct UserResponse {
    #[prost(message, repeated, tag = "1")]
    achievements: Vec<Unlock>,
    #[prost(int32, optional, tag = "2")]
    schema_version: Option<i32>,
    #[prost(uint32, optional, tag = "3")]
    schema_hash: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
struct Unlock {
    #[prost(uint32, optional, tag = "1")]
    key: Option<u32>,
    #[prost(bool, tag = "2")]
    unlocked: bool,
    #[prost(fixed32, tag = "3")]
    time: u32,
}

impl SteamConnection {
    pub(crate) async fn game_achievements(
        &mut self,
        app_id: u32,
        language: &str,
    ) -> Result<GameResponse, CmError> {
        self.service(
            "Player.GetGameAchievements#1",
            &GameRequest {
                app_id,
                language: language.into(),
            },
        )
        .await
    }

    pub(crate) async fn user_achievements(&mut self, app_id: u32) -> Result<UserResponse, CmError> {
        self.service(
            "Player.GetUserAchievements#1",
            &UserRequest {
                steam_id: self.steam_id,
                app_id,
            },
        )
        .await
    }
}

fn invalid() -> CmError {
    CmError::Protocol("backend.steam.achievements.invalidResponse".into())
}
pub(crate) fn combine(
    app_id: u32,
    schema: GameResponse,
    user: UserResponse,
) -> Result<Vec<Achievement>, CmError> {
    if schema.schema_hash != user.schema_hash || schema.schema_version != user.schema_version {
        return Err(invalid());
    }
    let mut states = HashMap::new();
    for state in user.achievements {
        let key = state.key.ok_or_else(invalid)?;
        if states.insert(key, state).is_some() {
            return Err(invalid());
        }
    }
    let mut ids = HashSet::new();
    let mut keys = HashSet::new();
    let mut result = Vec::new();
    for definition in schema.achievements {
        let key = definition.key.ok_or_else(invalid)?;
        if definition.id.is_empty() || !ids.insert(definition.id.clone()) || !keys.insert(key) {
            return Err(invalid());
        }
        // Drain an archived definition's state too, so the leftover check
        // below still only reports keys with no definition at all.
        let state = states.remove(&key).filter(|state| state.unlocked);
        if definition.archived {
            continue;
        }
        let unlocked = state.is_some();
        // A hidden achievement the account has not earned exposes its identity
        // and nothing else, so its icon is never even resolved to a URL.
        let hidden = definition.hidden && !unlocked;
        let (name, description, icon_url) = if hidden {
            (String::new(), String::new(), String::new())
        } else {
            let icon = if unlocked {
                definition.icon
            } else {
                definition.icon_gray
            };
            (
                definition.name,
                definition.description,
                achievement_icon(app_id, &icon),
            )
        };
        result.push(Achievement {
            id: definition.id,
            name,
            description,
            icon_url,
            hidden,
            unlocked,
            unlock_time: state.map_or(0, |state| state.time),
        });
    }
    // Unknown keys usually mean a schema changed between the two requests.
    if !states.is_empty() {
        return Err(invalid());
    }
    result.sort_by(|a, b| {
        b.unlocked
            .cmp(&a.unlocked)
            .then_with(|| b.unlock_time.cmp(&a.unlock_time))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(result)
}

fn achievement_icon(app_id: u32, icon: &str) -> String {
    if let Some((hash, extension)) = icon.rsplit_once('.')
        && hash.len() == 40
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(extension, "jpg" | "png")
    {
        return format!(
            "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/{app_id}/{icon}"
        );
    }
    reqwest::Url::parse(icon)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
        .map_or_else(String::new, |url| url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition(id: &str, key: u32) -> Definition {
        Definition {
            id: id.into(),
            name: id.into(),
            key: Some(key),
            ..Default::default()
        }
    }
    #[test]
    fn joins_by_key_hides_locked_secrets_and_omits_archived_achievements() {
        let schema = GameResponse {
            achievements: vec![
                Definition {
                    hidden: true,
                    description: "Spoiler".into(),
                    ..definition("secret", 2)
                },
                definition("earned", 1),
                Definition {
                    archived: true,
                    ..definition("old", 3)
                },
            ],
            ..Default::default()
        };
        let user = UserResponse {
            achievements: vec![Unlock {
                key: Some(1),
                unlocked: true,
                time: 123,
            }],
            ..Default::default()
        };
        let values = combine(620, schema, user).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].id, "earned");
        assert_eq!(values[0].unlock_time, 123);
        assert!(values[1].hidden);
        assert!(values[1].name.is_empty() && values[1].description.is_empty());
    }
    #[test]
    fn refuses_mismatched_schemas_duplicate_keys_and_unknown_user_records() {
        assert!(
            combine(
                620,
                GameResponse {
                    schema_hash: Some(1),
                    ..Default::default()
                },
                UserResponse {
                    schema_hash: Some(2),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            combine(
                620,
                GameResponse {
                    achievements: vec![definition("a", 1), definition("b", 1)],
                    ..Default::default()
                },
                UserResponse::default()
            )
            .is_err()
        );
        assert!(
            combine(
                620,
                GameResponse::default(),
                UserResponse {
                    achievements: vec![Unlock {
                        key: Some(1),
                        ..Default::default()
                    }],
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn queries_connected_account_and_propagates_service_denial() {
        use crate::cm::*;
        for denied in [false, true] {
            let (outgoing, mut commands) = mpsc::channel(8);
            let (responses, incoming) = mpsc::channel(8);
            let (_, profile) = watch::channel(ProfileState::default());
            let connection = SteamConnection {
                outgoing,
                incoming,
                profile,
                friends: friends::FriendsEventSource::new(42),
                worker: tokio::spawn(std::future::pending()),
                steam_id: 42,
                session_id: 1,
                cell_id: 0,
                next_job: 1,
                logged_on: true,
                licenses: None,
            };
            let client = crate::client::Client::with_test_connection(connection);
            let query = tokio::spawn(async move { client.achievements(620, "french").await });
            for (index, name) in [
                "Player.GetGameAchievements#1",
                "Player.GetUserAchievements#1",
            ]
            .into_iter()
            .enumerate()
            {
                let Command::Send(bytes) = timeout(Duration::from_secs(1), commands.recv())
                    .await
                    .unwrap()
                    .unwrap()
                else {
                    panic!("expected service request")
                };
                let mut packets = Vec::new();
                let mut budget = MAX_PACKET;
                decode_packets(bytes.into(), 0, &mut budget, &mut packets).unwrap();
                let packet = packets.remove(0);
                assert_eq!(packet.kind, SERVICE_REQUEST);
                assert_eq!(packet.header.target_name.as_deref(), Some(name));
                let body = if index == 0 {
                    let request = GameRequest::decode(packet.body.clone()).unwrap();
                    assert_eq!(request.app_id, 620);
                    assert_eq!(request.language, "french");
                    GameResponse {
                        achievements: vec![definition("won", 7)],
                        ..Default::default()
                    }
                    .encode_to_vec()
                } else {
                    let request = UserRequest::decode(packet.body.clone()).unwrap();
                    assert_eq!(request.steam_id, 42);
                    assert_eq!(request.app_id, 620);
                    UserResponse {
                        achievements: vec![Unlock {
                            key: Some(7),
                            unlocked: true,
                            time: 123,
                        }],
                        ..Default::default()
                    }
                    .encode_to_vec()
                };
                responses
                    .send(Ok(Packet {
                        kind: SERVICE_RESPONSE,
                        header: crate::proto::Header {
                            job_target: packet.header.job_source,
                            result: Some(if denied && index == 1 { 15 } else { 1 }),
                            ..Default::default()
                        },
                        body: body.into(),
                    }))
                    .await
                    .unwrap();
            }
            let result = query.await.unwrap();
            if denied {
                assert!(result.is_err());
            } else {
                let result = result.unwrap();
                assert_eq!(result.len(), 1);
                assert!(result[0].unlocked);
            }
        }
    }
    #[test]
    fn resolves_steam_icon_filenames_without_accepting_paths() {
        let file = "42d64a86bb4d50c2574612892ccdde465a020a54.jpg";
        assert_eq!(
            achievement_icon(1030300, file),
            format!(
                "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/1030300/{file}"
            )
        );
        assert!(achievement_icon(620, "../icon.jpg").is_empty());
        assert!(achievement_icon(620, "file:///private/icon.jpg").is_empty());
    }
}
