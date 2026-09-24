//! A local Steam CM for tests: the real connection worker over a local
//! WebSocket, with the test answering as Steam.
use super::*;

pub(super) struct TestServer(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>);

/// A connection for `steam_id`, already logged on when `logged_on`.
pub(super) async fn connection(steam_id: u64, logged_on: bool) -> (SteamConnection, TestServer) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (client, server) = tokio::join!(tokio_tungstenite::connect_async(endpoint), async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    });
    let mut connection = SteamConnection::start(client.unwrap().0, steam_id, logged_on);
    connection.session_id = 1;
    (connection, TestServer(server))
}

impl TestServer {
    /// The next message Forge sent, which must be of `kind`.
    pub(super) async fn request(&mut self, kind: u32) -> Packet {
        let packet = timeout(Duration::from_secs(1), self.next())
            .await
            .expect("Forge sent nothing");
        assert_eq!(packet.kind, kind);
        packet
    }

    async fn next(&mut self) -> Packet {
        loop {
            let frame = self
                .0
                .next()
                .await
                .expect("Forge closed the connection")
                .unwrap();
            let WsMessage::Binary(bytes) = frame else {
                continue;
            };
            let mut packets = Vec::new();
            decode_packets(bytes, 0, &mut { MAX_PACKET }, &mut packets).unwrap();
            assert_eq!(packets.len(), 1);
            let packet = packets.remove(0);
            if packet.kind != HEARTBEAT {
                return packet;
            }
        }
    }

    /// Answers `request` as Steam does, addressing its job.
    pub(super) async fn reply(&mut self, request: &Packet, kind: u32, body: &impl Message) {
        self.send(
            kind,
            proto::Header {
                job_target: request.header.job_source,
                result: Some(1),
                ..Default::default()
            },
            &body.encode_to_vec(),
        )
        .await;
    }

    pub(super) async fn send(&mut self, kind: u32, header: proto::Header, body: &[u8]) {
        self.0
            .send(WsMessage::Binary(encode_body(kind, &header, body).into()))
            .await
            .unwrap();
    }

    /// Forge sends nothing for a moment.
    pub(super) async fn quiet(&mut self) -> bool {
        timeout(Duration::from_millis(100), self.next())
            .await
            .is_err()
    }
}
