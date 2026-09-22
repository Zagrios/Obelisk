//! The only input containing secrets is the inherited, private stdin pipe.
use std::io::{Read, Write};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

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

/// Commands after the credential handoff. EOF still releases the native host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCommand {
    SetPresence { online: bool },
}
impl HostCommand {
    pub fn read(mut input: impl Read) -> std::io::Result<Option<Self>> {
        let mut byte = [0];
        match input.read_exact(&mut byte) {
            Ok(()) => match byte[0] {
                0 | 1 => Ok(Some(Self::SetPresence {
                    online: byte[0] == 1,
                })),
                _ => Err(invalid()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn write(self, mut output: impl Write) -> std::io::Result<()> {
        match self {
            Self::SetPresence { online } => output.write_all(&[u8::from(online)]),
        }
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
    let mut confirmation = exec_confirmation()?;
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
    #[test]
    fn presence_commands_preserve_the_host_pipe_until_eof() {
        let mut wire = Vec::new();
        HostCommand::SetPresence { online: true }
            .write(&mut wire)
            .unwrap();
        HostCommand::SetPresence { online: false }
            .write(&mut wire)
            .unwrap();
        assert_eq!(wire, [1, 0]);
        let mut input = wire.as_slice();
        assert_eq!(
            HostCommand::read(&mut input).unwrap(),
            Some(HostCommand::SetPresence { online: true })
        );
        assert_eq!(
            HostCommand::read(&mut input).unwrap(),
            Some(HostCommand::SetPresence { online: false })
        );
        assert_eq!(HostCommand::read(&mut input).unwrap(), None);
        assert!(HostCommand::read(&[2][..]).is_err());
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
    fn protocol_rejects_native_output_and_extra_private_fields() {
        assert!(serde_json::from_str::<Event>("native token=secret").is_err());
        assert!(
            serde_json::from_str::<Event>(r#"{"event":"authenticated","token":"secret"}"#).is_err()
        );
    }
}
