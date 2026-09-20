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
    SdkVerified {
        app_id: u32,
        asynchronous_request: bool,
    },
    SdkStage {
        stage: SdkStage,
    },
    Disconnected {},
    Reconnected {},
    Failed {
        reason: Failure,
    },
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SdkStage {
    Library,
    Initialize,
    Engine,
    Interfaces,
    Account,
    AsynchronousRequest,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Engine,
    Authentication,
    Ownership,
    Discovery,
    Sdk,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
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
