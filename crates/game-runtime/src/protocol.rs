//! The only input containing secrets is the inherited, private stdin pipe.
use obelisk_core::OnlineStatus;
use std::io::{Read, Write};
use std::time::Duration;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// The native Steam host's bounded startup budget. A cold library can need
/// over a minute to publish its licenses after authentication succeeds.
pub const HOST_STARTUP_TIMEOUT: Duration = Duration::from_secs(180);
/// What the supervisor waits for the host. Its clock starts at spawn and the
/// host's only after it has loaded the engine and logged in, and a verdict
/// still has to travel back: the margin covers both.
pub const SUPERVISOR_STARTUP_TIMEOUT: Duration =
    HOST_STARTUP_TIMEOUT.saturating_add(Duration::from_secs(10));
// A budget under a minute fails the cold libraries it exists for.
const _: () = assert!(HOST_STARTUP_TIMEOUT.as_secs() > 60);

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Credentials {
    pub steam_id: u64,
    pub account_name: String,
    pub refresh_token: String,
}
impl Credentials {
    pub fn valid(&self) -> bool {
        self.steam_id != 0
            && (1..=64).contains(&self.account_name.len())
            && (1..=32768).contains(&self.refresh_token.len())
            && !self.account_name.contains('\0')
            && !self.refresh_token.contains('\0')
    }
    pub fn write(&self, mut output: impl Write) -> std::io::Result<()> {
        if !self.valid() {
            return Err(invalid());
        }
        output.write_all(&self.steam_id.to_le_bytes())?;
        output.write_all(&(self.refresh_token.len() as u32).to_le_bytes())?;
        output.write_all(&(self.account_name.len() as u32).to_le_bytes())?;
        output.write_all(self.refresh_token.as_bytes())?;
        output.write_all(self.account_name.as_bytes())
    }
    pub fn read(mut input: impl Read) -> std::io::Result<Self> {
        let mut header = [0; 16];
        input.read_exact(&mut header)?;
        let steam_id = u64::from_le_bytes(header[..8].try_into().map_err(|_| invalid())?);
        let token_length =
            u32::from_le_bytes(header[8..12].try_into().map_err(|_| invalid())?) as usize;
        let account_length =
            u32::from_le_bytes(header[12..].try_into().map_err(|_| invalid())?) as usize;
        if steam_id == 0
            || !(1..=32768).contains(&token_length)
            || !(1..=64).contains(&account_length)
        {
            return Err(invalid());
        }
        let mut token = Zeroizing::new(vec![0; token_length]);
        let mut account = Zeroizing::new(vec![0; account_length]);
        input.read_exact(&mut token)?;
        input.read_exact(&mut account)?;
        let credentials = Self {
            steam_id,
            refresh_token: std::str::from_utf8(&token).map_err(|_| invalid())?.into(),
            account_name: std::str::from_utf8(&account).map_err(|_| invalid())?.into(),
        };
        if !credentials.valid() {
            return Err(invalid());
        }
        Ok(credentials)
    }
}
fn invalid() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Invalid Steam session handoff",
    )
}

/// Commands after the credential handoff, one frame each: the protocol
/// version, a tag, the payload's little-endian `u16` length, then the payload.
/// A frame of another version, or longer than `MAX_PAYLOAD`, ends the stream;
/// a well-framed command the host does not know, or whose payload it refuses,
/// is skipped. EOF between frames still releases the native host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCommand {
    /// Shows the account as `status` says while its game runs, or offline
    /// once the game stopped (`None`). On the wire, Steam's persona state:
    /// 0 for `None`, which the host's engine is told as it is.
    SetPresence { status: Option<OnlineStatus> },
    /// Tells the game Steam's overlay opened or closed
    /// (`GameOverlayActivated_t`), as Obelisk's own overlay does.
    OverlayState { active: bool, user_initiated: bool },
    /// Hands the running game a lobby invitation the player accepted
    /// (`GameLobbyJoinRequested_t`).
    AcceptLobbyInvite { lobby_id: u64, friend: u64 },
    /// Hands the running game a connect-string invitation the player
    /// accepted (`GameRichPresenceJoinRequested_t`).
    AcceptRichInvite { friend: u64, connect: String },
    /// The player's answer to the purchase the game asked Steam to confirm.
    AuthorizeMicroTxn { order_id: u64, approve: bool },
}
impl HostCommand {
    pub const VERSION: u8 = 1;
    /// Bounds every frame: the largest command is a connect string.
    pub const MAX_PAYLOAD: usize = 512;
    const SET_PRESENCE: u8 = 1;
    const OVERLAY_STATE: u8 = 2;
    const ACCEPT_LOBBY_INVITE: u8 = 3;
    const ACCEPT_RICH_INVITE: u8 = 4;
    const AUTHORIZE_MICRO_TXN: u8 = 5;

    /// The persona state Steam shows for `status`; offline for `None`.
    pub fn persona_state(status: Option<OnlineStatus>) -> u8 {
        status.map_or(0, steam_kit::friends::persona_state)
    }
    /// The status a persona state byte stands for, if Obelisk sends it.
    fn status_of(byte: u8) -> Option<Option<OnlineStatus>> {
        [
            None,
            Some(OnlineStatus::Online),
            Some(OnlineStatus::Away),
            Some(OnlineStatus::Invisible),
        ]
        .into_iter()
        .find(|status| Self::persona_state(*status) == byte)
    }

    /// The next command, `None` at EOF between frames.
    pub fn read(mut input: impl Read) -> std::io::Result<Option<Self>> {
        loop {
            let mut header = [0; 4];
            match input.read_exact(&mut header[..1]) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
            input.read_exact(&mut header[1..])?;
            let length = usize::from(u16::from_le_bytes([header[2], header[3]]));
            if header[0] != Self::VERSION || length > Self::MAX_PAYLOAD {
                return Err(invalid());
            }
            let mut payload = vec![0; length];
            input.read_exact(&mut payload)?;
            if let Some(command) = Self::decode(header[1], &payload) {
                return Ok(Some(command));
            }
        }
    }
    pub fn write(&self, mut output: impl Write) -> std::io::Result<()> {
        let (tag, payload) = self.encode().ok_or_else(invalid)?;
        let length = u16::try_from(payload.len()).map_err(|_| invalid())?;
        let mut frame = vec![Self::VERSION, tag];
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(&payload);
        output.write_all(&frame)
    }
    /// Whether the host may carry this command out as it stands.
    pub fn valid(&self) -> bool {
        match self {
            Self::SetPresence { .. } | Self::OverlayState { .. } => true,
            Self::AcceptLobbyInvite { lobby_id, friend } => lobby(*lobby_id) && individual(*friend),
            Self::AcceptRichInvite { friend, connect } => {
                individual(*friend) && self::connect(connect)
            }
            Self::AuthorizeMicroTxn { order_id, .. } => *order_id != 0,
        }
    }
    fn encode(&self) -> Option<(u8, Vec<u8>)> {
        if !self.valid() {
            return None;
        }
        Some(match self {
            Self::SetPresence { status } => {
                (Self::SET_PRESENCE, vec![Self::persona_state(*status)])
            }
            Self::OverlayState {
                active,
                user_initiated,
            } => (
                Self::OVERLAY_STATE,
                vec![u8::from(*active), u8::from(*user_initiated)],
            ),
            Self::AcceptLobbyInvite { lobby_id, friend } => (
                Self::ACCEPT_LOBBY_INVITE,
                [lobby_id.to_le_bytes(), friend.to_le_bytes()].concat(),
            ),
            Self::AcceptRichInvite { friend, connect } => (
                Self::ACCEPT_RICH_INVITE,
                [&friend.to_le_bytes()[..], connect.as_bytes()].concat(),
            ),
            Self::AuthorizeMicroTxn { order_id, approve } => (
                Self::AUTHORIZE_MICRO_TXN,
                [&order_id.to_le_bytes()[..], &[u8::from(*approve)]].concat(),
            ),
        })
    }
    fn decode(tag: u8, payload: &[u8]) -> Option<Self> {
        let flag = |byte: u8| match byte {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let id = |at: usize| {
            Some(u64::from_le_bytes(
                payload.get(at..at + 8)?.try_into().ok()?,
            ))
        };
        let command = match (tag, payload.len()) {
            (Self::SET_PRESENCE, 1) => Self::SetPresence {
                status: Self::status_of(payload[0])?,
            },
            (Self::OVERLAY_STATE, 2) => Self::OverlayState {
                active: flag(payload[0])?,
                user_initiated: flag(payload[1])?,
            },
            (Self::ACCEPT_LOBBY_INVITE, 16) => Self::AcceptLobbyInvite {
                lobby_id: id(0)?,
                friend: id(8)?,
            },
            (Self::ACCEPT_RICH_INVITE, 9..) => Self::AcceptRichInvite {
                friend: id(0)?,
                connect: std::str::from_utf8(&payload[8..]).ok()?.to_owned(),
            },
            (Self::AUTHORIZE_MICRO_TXN, 9) => Self::AuthorizeMicroTxn {
                order_id: id(0)?,
                approve: flag(payload[8])?,
            },
            _ => return None,
        };
        command.valid().then_some(command)
    }
}

/// Reply of an exec helper whose preparation or exec failed. A helper that
/// replaced itself with the game instead closes its confirmation pipe silently.
pub const EXEC_FAILED: &[u8] = b"exec-failed\n";

/// Runs an exec helper's preparation and exec. The supervisor treats EOF as a
/// successful exec, which never returns here; every other outcome sends
/// `EXEC_FAILED` before the confirmation pipe closes.
pub fn exec_or_report<E: From<std::io::Error>>(
    exec: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    exec_or_report_to(exec_confirmation()?, exec)
}

/// `confirmation` must close on exec.
fn exec_or_report_to<E>(
    mut confirmation: std::fs::File,
    exec: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    let result = exec();
    let _ = confirmation.write_all(EXEC_FAILED);
    result
}

/// A CLOEXEC duplicate of stdout that closes atomically on a successful exec.
/// When even that fails, the marker still reaches the supervisor through stdout.
fn exec_confirmation() -> std::io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    let stdout = std::io::stdout();
    stdout
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .inspect_err(|_| {
            let _ = stdout.lock().write_all(EXEC_FAILED);
        })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Authenticated {},
    Ready {
        app_id: u32,
        host_pid: u32,
        ticket_bytes: u32,
    },
    Discovery {
        paths: u32,
    },
    Disconnected {},
    Reconnected {},
    Failed {
        reason: Failure,
    },
    /// The game asked Steam's overlay for `request`, which this host cannot
    /// show: Obelisk shows its own instead.
    OverlayRequested {
        app_id: u32,
        request: OverlayDialog,
    },
    /// A friend invited the account into a lobby of game `app_id`
    /// (`LobbyInvite_t`).
    LobbyInviteReceived {
        inviter: u64,
        lobby_id: u64,
        app_id: u32,
    },
    /// A friend invited the account to join them with a connect string
    /// (`RichInviteReceived_t`, internal).
    RichInviteReceived {
        friend: u64,
        app_id: u32,
        connect: String,
    },
    /// Game `app_id` asks the player to confirm purchase `order_id`
    /// (`MicroTxnAuthRequestCallback_t`, internal).
    MicroTxnRequested {
        order_id: u64,
        app_id: u32,
    },
    /// Steam no longer waits for an answer to purchase `order_id`.
    MicroTxnDismissed {
        order_id: u64,
    },
    /// The lobby the account's Steam engine shows it in while playing
    /// `app_id`, as the host last read it; `None` once it is in none.
    GameLobby {
        app_id: u32,
        lobby_id: Option<u64>,
    },
}

/// What a game asked to invite friends into: its Steam lobby, or the
/// command line a friend's game starts with.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum InviteTarget {
    Lobby { lobby_id: u64 },
    Connect { connect: String },
}
impl InviteTarget {
    /// Steam's own limit on a connect string, without its terminating NUL.
    pub const MAX_CONNECT: usize = 255;

    /// A lobby is a Chat SteamID; a connect string fits Steam's buffer.
    pub fn valid(&self) -> bool {
        match self {
            Self::Lobby { lobby_id } => lobby(*lobby_id),
            Self::Connect { connect } => self::connect(connect),
        }
    }
}

/// What a game asked Steam's overlay to show (`GameOverlayActivateRequested_t`,
/// 1010001), one variant per `ISteamFriends::ActivateGameOverlay*` call.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OverlayDialog {
    /// `ActivateGameOverlay(dialog)`: "friends", "community", "players",
    /// "settings", "officialgamegroup", "stats", "achievements",
    /// "chatroomgroup/<id>", …
    Dialog { dialog: String },
    /// `ActivateGameOverlayToUser(dialog, steam_id)`: "steamid", "chat",
    /// "jointrade", "stats", "achievements", "friendadd", …
    User { dialog: String, steam_id: u64 },
    /// `ActivateGameOverlayToWebPage(url, mode)`; `modal` is
    /// `k_EActivateGameOverlayToWebPageMode_Modal`.
    WebPage { url: String, modal: bool },
    /// `ActivateGameOverlayToStore(app_id, flag)`; `flag` is
    /// `EOverlayToStoreFlag`: 0 none, 1 add to cart, 2 add to cart and show.
    Store { app_id: u32, flag: u32 },
    /// `ActivateGameOverlayInviteDialog(lobby)`.
    LobbyInvite { lobby_id: u64 },
    /// `ActivateGameOverlayRemotePlayTogetherInviteDialog(lobby)`.
    RemotePlayTogetherInvite { lobby_id: Option<u64> },
    /// `ActivateGameOverlayInviteDialogConnectString(connect)`.
    ConnectInvite { connect: String },
    /// Steam's own Remote Play Together dialog.
    RemotePlayTogether {},
}
impl OverlayDialog {
    /// Longest dialog name Obelisk accepts, such as "chatroomgroup/<id>".
    pub const MAX_DIALOG: usize = 128;
    /// Longest web page URL Obelisk accepts: the event stays far below the
    /// host's line limit.
    pub const MAX_URL: usize = 2048;
    /// The highest `EOverlayToStoreFlag`.
    pub const MAX_STORE_FLAG: u32 = 2;

    /// Each field within Steam's own limits and Obelisk's.
    pub fn valid(&self) -> bool {
        let dialog = |dialog: &str| {
            (1..=Self::MAX_DIALOG).contains(&dialog.len())
                && dialog
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-./".contains(&byte))
        };
        match self {
            Self::Dialog { dialog: name } => dialog(name),
            Self::User {
                dialog: name,
                steam_id,
            } => dialog(name) && *steam_id != 0,
            Self::WebPage { url, .. } => {
                url.len() <= Self::MAX_URL
                    && (url.starts_with("https://") || url.starts_with("http://"))
                    && url
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
            }
            Self::Store { flag, .. } => *flag <= Self::MAX_STORE_FLAG,
            Self::LobbyInvite { lobby_id } => lobby(*lobby_id),
            Self::RemotePlayTogetherInvite { lobby_id } => lobby_id.is_none_or(lobby),
            Self::ConnectInvite { connect } => self::connect(connect),
            Self::RemotePlayTogether {} => true,
        }
    }
    /// The invitation an invite dialog asks for; every other dialog has none.
    pub fn invite(&self) -> Option<InviteTarget> {
        match self {
            Self::LobbyInvite { lobby_id } => Some(InviteTarget::Lobby {
                lobby_id: *lobby_id,
            }),
            Self::ConnectInvite { connect } => Some(InviteTarget::Connect {
                connect: connect.clone(),
            }),
            _ => None,
        }
    }
}

/// A Steam lobby's SteamID: account type Chat (8), in a real universe, with
/// an account number.
pub fn lobby(steam_id: u64) -> bool {
    (steam_id >> 52) & 0xf == 8 && steam_id >> 56 != 0 && steam_id as u32 != 0
}

/// A person's SteamID: account type Individual (1), in a real universe, with
/// an account number.
pub fn individual(steam_id: u64) -> bool {
    (steam_id >> 52) & 0xf == 1 && steam_id >> 56 != 0 && steam_id as u32 != 0
}

/// A rich presence connect string that fits Steam's buffer.
pub fn connect(connect: &str) -> bool {
    !connect.is_empty() && connect.len() <= InviteTarget::MAX_CONNECT && !connect.contains('\0')
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Engine,
    Authentication,
    Ownership,
    Discovery,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    const LOBBY: u64 = 109_775_244_421_126_053;
    const FRIEND: u64 = 76_561_197_960_265_729;

    fn commands() -> Vec<HostCommand> {
        vec![
            HostCommand::SetPresence {
                status: Some(OnlineStatus::Online),
            },
            HostCommand::SetPresence { status: None },
            HostCommand::SetPresence {
                status: Some(OnlineStatus::Away),
            },
            HostCommand::SetPresence {
                status: Some(OnlineStatus::Invisible),
            },
            HostCommand::OverlayState {
                active: true,
                user_initiated: false,
            },
            HostCommand::AcceptLobbyInvite {
                lobby_id: LOBBY,
                friend: FRIEND,
            },
            HostCommand::AcceptRichInvite {
                friend: FRIEND,
                connect: "+connect_lobby 109775244421126053".into(),
            },
            HostCommand::AuthorizeMicroTxn {
                order_id: 4_242,
                approve: false,
            },
        ]
    }

    #[test]
    fn host_commands_round_trip_framed_until_eof() {
        let mut wire = Vec::new();
        for command in commands() {
            command.write(&mut wire).unwrap();
        }
        assert_eq!(&wire[..5], [1, 1, 1, 0, 1], "version, tag, length, online");
        assert_eq!(&wire[5..10], [1, 1, 1, 0, 0], "offline");
        assert_eq!(&wire[10..15], [1, 1, 1, 0, 3], "away");
        assert_eq!(&wire[15..20], [1, 1, 1, 0, 7], "invisible");
        assert_eq!(&wire[20..26], [1, 2, 2, 0, 1, 0]);
        let mut input = wire.as_slice();
        for command in commands() {
            assert_eq!(HostCommand::read(&mut input).unwrap(), Some(command));
        }
        assert_eq!(HostCommand::read(&mut input).unwrap(), None);
    }

    #[test]
    fn host_commands_skip_what_they_refuse_and_stop_on_a_broken_frame() {
        let frame = |tag: u8, payload: &[u8]| {
            let mut frame = vec![HostCommand::VERSION, tag];
            frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            frame.extend_from_slice(payload);
            frame
        };
        let mut wire = Vec::new();
        for skipped in [
            frame(9, b"future"),
            // Busy and the states Obelisk never sends.
            frame(1, &[2]),
            frame(1, &[8]),
            frame(1, &[1, 0]),
            frame(2, &[1, 7]),
            // An account is no lobby, a lobby is no friend.
            frame(3, &[FRIEND.to_le_bytes(), FRIEND.to_le_bytes()].concat()),
            frame(3, &[LOBBY.to_le_bytes(), LOBBY.to_le_bytes()].concat()),
            frame(4, &FRIEND.to_le_bytes()),
            frame(4, &[&FRIEND.to_le_bytes()[..], &[0xff]].concat()),
            frame(4, &[&FRIEND.to_le_bytes()[..], b"a\0b"].concat()),
            frame(4, &[&FRIEND.to_le_bytes()[..], &[b'a'; 256]].concat()),
            frame(5, &[0; 9]),
            frame(5, &[&1_u64.to_le_bytes()[..], &[2]].concat()),
        ] {
            wire.extend_from_slice(&skipped);
        }
        let online = HostCommand::SetPresence {
            status: Some(OnlineStatus::Online),
        };
        online.write(&mut wire).unwrap();
        let mut input = wire.as_slice();
        assert_eq!(HostCommand::read(&mut input).unwrap(), Some(online));
        assert_eq!(HostCommand::read(&mut input).unwrap(), None);

        assert!(
            HostCommand::read(&frame(1, &[1])[..3]).is_err(),
            "truncated"
        );
        let mut version = frame(1, &[1]);
        version[0] = 2;
        assert!(HostCommand::read(version.as_slice()).is_err());
        let mut long = vec![HostCommand::VERSION, 4];
        long.extend_from_slice(&513_u16.to_le_bytes());
        long.resize(4 + 513, b'a');
        assert!(HostCommand::read(long.as_slice()).is_err());
        assert!(
            HostCommand::AcceptRichInvite {
                friend: FRIEND,
                connect: "a".repeat(256),
            }
            .write(Vec::new())
            .is_err(),
            "nothing invalid is written"
        );
        assert!(
            HostCommand::AcceptRichInvite {
                friend: FRIEND,
                connect: "a".repeat(255),
            }
            .write(Vec::new())
            .is_ok()
        );
    }

    #[test]
    fn overlay_requests_round_trip_and_reject_invalid_fields() {
        let event = Event::OverlayRequested {
            app_id: 3527290,
            request: OverlayDialog::LobbyInvite { lobby_id: LOBBY },
        };
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(
            line,
            r#"{"event":"overlay_requested","app_id":3527290,"request":{"lobby_invite":{"lobby_id":109775244421126053}}}"#
        );
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), event);
        assert!(
            serde_json::from_str::<Event>(
                r#"{"event":"overlay_requested","app_id":1,"request":{"lobby_invite":{"lobby_id":1,"x":2}}}"#
            )
            .is_err()
        );
        let line =
            r#"{"event":"overlay_requested","app_id":1,"request":{"remote_play_together":{}}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(line).unwrap(),
            Event::OverlayRequested {
                app_id: 1,
                request: OverlayDialog::RemotePlayTogether {}
            }
        );
        // PEAK's lobby from its Player.log: universe 1, Chat, lobby instance.
        assert!(lobby(LOBBY));
        assert!(!lobby(FRIEND), "an individual account");
        assert!(!lobby(0x0080_0000_0000_0001), "no universe");
        assert!(!lobby(0x0186_0000_0000_0000), "no account number");
        assert!(individual(FRIEND));
        assert!(!individual(LOBBY) && !individual(0) && !individual(FRIEND & !0xffff_ffff));
        assert!(!individual(FRIEND & ((1 << 56) - 1)), "no universe");
        for valid in [
            OverlayDialog::Dialog {
                dialog: "chatroomgroup/123".into(),
            },
            OverlayDialog::User {
                dialog: "steamid".into(),
                steam_id: FRIEND,
            },
            OverlayDialog::WebPage {
                url: "https://store.steampowered.com/app/620".into(),
                modal: true,
            },
            OverlayDialog::Store {
                app_id: 620,
                flag: 2,
            },
            OverlayDialog::RemotePlayTogetherInvite { lobby_id: None },
            OverlayDialog::ConnectInvite {
                connect: "+connect 10.0.0.1".into(),
            },
        ] {
            assert!(valid.valid(), "{valid:?}");
        }
        for invalid in [
            OverlayDialog::Dialog {
                dialog: String::new(),
            },
            OverlayDialog::Dialog {
                dialog: "a b".into(),
            },
            OverlayDialog::Dialog {
                dialog: "a".repeat(129),
            },
            OverlayDialog::User {
                dialog: "chat".into(),
                steam_id: 0,
            },
            OverlayDialog::WebPage {
                url: "file:///etc/passwd".into(),
                modal: false,
            },
            OverlayDialog::WebPage {
                url: "steam://run/620".into(),
                modal: false,
            },
            OverlayDialog::WebPage {
                url: "https://a b".into(),
                modal: false,
            },
            OverlayDialog::WebPage {
                url: format!("https://{}", "a".repeat(2041)),
                modal: false,
            },
            OverlayDialog::Store { app_id: 1, flag: 3 },
            OverlayDialog::LobbyInvite { lobby_id: FRIEND },
            OverlayDialog::RemotePlayTogetherInvite {
                lobby_id: Some(FRIEND),
            },
            OverlayDialog::ConnectInvite {
                connect: String::new(),
            },
        ] {
            assert!(!invalid.valid(), "{invalid:?}");
        }
        assert!(
            InviteTarget::Connect {
                connect: "+connect_lobby 1".into()
            }
            .valid()
        );
        for connect in [String::new(), "a".repeat(256), "a\0b".into()] {
            assert!(!InviteTarget::Connect { connect }.valid());
        }
    }

    #[test]
    fn received_invites_and_purchases_round_trip() {
        for (event, line) in [
            (
                Event::LobbyInviteReceived {
                    inviter: FRIEND,
                    lobby_id: LOBBY,
                    app_id: 3527290,
                },
                r#"{"event":"lobby_invite_received","inviter":76561197960265729,"lobby_id":109775244421126053,"app_id":3527290}"#,
            ),
            (
                Event::RichInviteReceived {
                    friend: FRIEND,
                    app_id: 620,
                    connect: "+connect 10.0.0.1".into(),
                },
                r#"{"event":"rich_invite_received","friend":76561197960265729,"app_id":620,"connect":"+connect 10.0.0.1"}"#,
            ),
            (
                Event::MicroTxnRequested {
                    order_id: 4242,
                    app_id: 620,
                },
                r#"{"event":"micro_txn_requested","order_id":4242,"app_id":620}"#,
            ),
            (
                Event::MicroTxnDismissed { order_id: 4242 },
                r#"{"event":"micro_txn_dismissed","order_id":4242}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&event).unwrap(), line);
            assert_eq!(serde_json::from_str::<Event>(line).unwrap(), event);
        }
        assert!(
            serde_json::from_str::<Event>(
                r#"{"event":"micro_txn_dismissed","order_id":1,"price":2}"#
            )
            .is_err()
        );
    }
    #[test]
    fn session_packet_is_bounded_and_rejects_truncation_and_nul() {
        let session = Credentials {
            steam_id: 123,
            account_name: "test-user".into(),
            refresh_token: "test-token".into(),
        };
        let mut packet = Zeroizing::new(Vec::new());
        session.write(&mut *packet).unwrap();
        let restored = Credentials::read(packet.as_slice()).unwrap();
        assert_eq!(restored.steam_id, 123);
        assert_eq!(restored.refresh_token, "test-token");
        assert!(Credentials::read(&packet[..packet.len() - 1]).is_err());
        packet[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Credentials::read(packet.as_slice()).is_err());
        let bad = Credentials {
            steam_id: 123,
            account_name: "test\0user".into(),
            refresh_token: "token".into(),
        };
        assert!(bad.write(Vec::new()).is_err());
    }
    #[test]
    fn game_lobby_events_carry_the_lobby_or_none() {
        for (event, line) in [
            (
                Event::GameLobby {
                    app_id: 3527290,
                    lobby_id: Some(109_775_244_421_126_053),
                },
                r#"{"event":"game_lobby","app_id":3527290,"lobby_id":109775244421126053}"#,
            ),
            (
                Event::GameLobby {
                    app_id: 3527290,
                    lobby_id: None,
                },
                r#"{"event":"game_lobby","app_id":3527290,"lobby_id":null}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&event).unwrap(), line);
            assert_eq!(serde_json::from_str::<Event>(line).unwrap(), event);
        }
        assert!(
            serde_json::from_str::<Event>(
                r#"{"event":"game_lobby","app_id":1,"lobby_id":null,"connect":"x"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn protocol_rejects_native_output_and_extra_private_fields() {
        assert!(serde_json::from_str::<Event>("native token=secret").is_err());
        assert!(
            serde_json::from_str::<Event>(r#"{"event":"authenticated","token":"secret"}"#).is_err()
        );
    }
}
