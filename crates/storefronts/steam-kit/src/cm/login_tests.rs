use super::*;

// Decode only the independently specified session-disambiguation wire fields.
// CMsgClientLogon: legacy uint32 field 2, CMsgIPAddress field 11, fixed32 v4.
#[derive(Clone, PartialEq, prost::Message)]
struct WireLogin {
    #[prost(uint32, optional, tag = "2")]
    legacy: Option<u32>,
    #[prost(message, optional, tag = "11")]
    address: Option<WireAddress>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct WireAddress {
    #[prost(fixed32, optional, tag = "1")]
    v4: Option<u32>,
}

async fn logon_request() -> (SteamConnection, WireLogin) {
    const ACCOUNT: u64 = 0x0110_0001_0000_002a;
    let (outgoing, mut commands) = mpsc::channel(32);
    let (responses, incoming) = mpsc::channel(1);
    let (_, profile) = watch::channel(ProfileState::default());
    responses
        .send(Ok(Packet {
            kind: LOGON_RESPONSE,
            header: proto::Header {
                steam_id: Some(ACCOUNT),
                session_id: Some(42),
                ..Default::default()
            },
            body: proto::LogonResponse {
                result: Some(1),
                ..Default::default()
            }
            .encode_to_vec()
            .into(),
        }))
        .await
        .unwrap();
    let mut connection = SteamConnection {
        outgoing,
        incoming,
        profile,
        friends: friends::FriendsEventSource::new(0),
        worker: tokio::spawn(std::future::pending()),
        steam_id: 0,
        session_id: 0,
        cell_id: 0,
        next_job: 1,
        logged_on: false,
        licenses: None,
    };
    connection
        .log_on("fixture", ACCOUNT, "fixture-token")
        .await
        .unwrap();
    assert!(matches!(
        commands.recv().await,
        Some(Command::Account(ACCOUNT))
    ));
    let Some(Command::Send(bytes)) = commands.recv().await else {
        panic!("missing logon request")
    };
    let mut packets = Vec::new();
    decode_packets(bytes.into(), 0, &mut { MAX_PACKET }, &mut packets).unwrap();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].kind, LOGON);
    let wire = WireLogin::decode(&packets[0].body[..]).unwrap();
    (connection, wire)
}

#[tokio::test]
async fn concurrent_logons_for_one_account_do_not_share_the_default_login_id() {
    // Keep the first physical session alive while admitting the Cloud session.
    let (_main_session, main) = logon_request().await;
    let (_cloud_session, cloud) = logon_request().await;
    let mut ids = std::collections::HashSet::new();
    for login in [main, cloud] {
        let id = login
            .address
            .and_then(|ip| ip.v4)
            .expect("missing login ID: concurrent Steam sessions would replace each other");
        assert_ne!(id, 0);
        assert_eq!(login.legacy, Some(id));
        assert!(
            ids.insert(id),
            "physical connections reused the same login ID"
        );
    }
}
