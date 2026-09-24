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
    let (mut connection, mut server) = test_server::connection(0, false).await;
    let steam = async {
        let logon = server.request(LOGON).await;
        server
            .send(
                LOGON_RESPONSE,
                proto::Header {
                    steam_id: Some(ACCOUNT),
                    session_id: Some(42),
                    ..Default::default()
                },
                &proto::LogonResponse {
                    result: Some(1),
                    ..Default::default()
                }
                .encode_to_vec(),
            )
            .await;
        WireLogin::decode(&logon.body[..]).unwrap()
    };
    let (logged_on, wire) = tokio::join!(
        connection.log_on("fixture", ACCOUNT, "fixture-token"),
        steam
    );
    logged_on.unwrap();
    assert_eq!(connection.session_id, 42);
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
