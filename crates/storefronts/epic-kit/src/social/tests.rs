//! Loopback protocol fixtures exercise Client, HTTP, WebSocket and social watch together.

use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

use crate::{
    auth::{Api, Credentials, Session},
    client::Client,
    friends::{Connection, Presence, Snapshot},
};

const SAFETY_BOUND: Duration = Duration::from_secs(5);
const SUMMARY_PATH: &str = "/friends/api/v1/account-a/summary?displayNames=true";
const REGISTRATION_PATH: &str =
    "/epic/presence/internal/v1/_/account-a/presence/fixture-connection";
const PROFILES_PATH: &str = "/account/api/public/account";
const ONLINE: &str =
    r#"{"type":"presence.v1.UPDATE","payload":{"accountId":"alice","status":"online","perNs":[]}}"#;
const AWAY: &str =
    r#"{"type":"presence.v1.UPDATE","payload":{"accountId":"alice","status":"away","perNs":[]}}"#;
const PLAYING: &str = r#"{"type":"presence.v1.UPDATE","payload":{"accountId":"alice","status":"online","perNs":[{"ns":"_","status":"online","conns":[{"runningApp":{"id":"game-a","displayName":"Fixture Game"}}]}]}}"#;
const REMOVED: &str =
    r#"{"type":"friends.v1.FRIEND_ENTRY_REMOVED","payload":{"from":"account-a","to":"alice"}}"#;

#[tokio::test]
async fn running_game_is_advertised_on_its_own_connection_and_reconnect() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client
        .start_game_presence(crate::friends::Activity {
            id: "kingdom-namespace".into(),
            name: Some("Kingdom: New Lands".into()),
        })
        .unwrap();
    let mut peer = fixture.peer().await;
    for reconnect in [false, true] {
        if reconnect {
            peer.socket.close(None).await.unwrap();
            peer = fixture.peer().await;
        }
        peer.subscribe().await;
        let registration = fixture.registration().await;
        let body: Value = serde_json::from_str(&registration.request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "status": "online",
                "conn": {"props": {}, "runningApp": {
                    "id": "kingdom-namespace",
                    "displayName": "Kingdom: New Lands"
                }}
            })
        );
        registration.respond(200, registration_response());
        state(&client, |s| s.connection == Connection::Ready).await;
        peer.event(REMOVED).await;
        peer.barrier().await;
        assert!(
            fixture.http.try_recv().is_err(),
            "game presence must not read friends"
        );
    }
    client.stop_friends();
    peer.closed().await;
    assert_eq!(client.friends().connection, Connection::Stopped);
    // A later social connection must not inherit the exited game's activity.
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, registration) = fixture.bootstrap().await;
    assert_eq!(
        serde_json::from_str::<Value>(&registration.request.body).unwrap(),
        json!({"status":"online","conn":{"props":{}}})
    );
    summary_request.respond(200, summary());
    registration.respond(200, registration_response());
    state(&client, |s| s.connection == Connection::Ready).await;
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn game_presence_recovers_after_repeated_network_failures_until_stopped() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client
        .start_game_presence(crate::friends::Activity {
            id: "game".into(),
            name: Some("Fixture Game".into()),
        })
        .unwrap();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    fixture
        .registration()
        .await
        .respond(200, registration_response());
    state(&client, |s| s.connection == Connection::Ready).await;

    for delay in [1, 2, 4, 8, 16, 30, 30] {
        peer.socket.close(None).await.unwrap();
        let interrupted = state(&client, |s| s.connection != Connection::Ready).await;
        assert_eq!(interrupted.connection, Connection::Interrupted);
        advance_time(Duration::from_secs(delay)).await;
        peer = fixture.peer().await;
        peer.subscribe().await;
        let registration = fixture.registration().await;
        let body: Value = serde_json::from_str(&registration.request.body).unwrap();
        assert_eq!(
            body["conn"]["runningApp"],
            json!({"id":"game", "displayName":"Fixture Game"})
        );
        registration.respond(200, registration_response());
        state(&client, |s| s.connection == Connection::Ready).await;
    }

    client.stop_friends();
    peer.closed().await;
    advance_time(Duration::from_secs(300)).await;
    assert_eq!(client.friends().connection, Connection::Stopped);
    assert!(fixture.peers.try_recv().is_err());
    assert!(fixture.http.try_recv().is_err());
}

#[tokio::test]
async fn game_presence_recovers_from_a_transient_token_refresh_failure() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client
        .start_game_presence(crate::friends::Activity {
            id: "game".into(),
            name: None,
        })
        .unwrap();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    fixture.registration().await.respond(401, json!({}));
    peer.closed().await;
    // Close the transport before the refresh receives a response.
    drop(fixture.renewal().await);
    let interrupted = state(&client, |s| {
        s.message.as_deref() == Some("backend.epic.friends.network")
    })
    .await;
    assert_eq!(interrupted.connection, Connection::Interrupted);
    advance_time(Duration::from_secs(1)).await;
    fixture.renewal().await.respond(200, renewed_token());

    let mut peer = fixture.peer().await;
    peer.subscribe_with_token("fixture-access-new").await;
    let registration = fixture.registration().await;
    assert_eq!(
        registration.request.headers["authorization"],
        "Bearer fixture-access-new"
    );
    let body: Value = serde_json::from_str(&registration.request.body).unwrap();
    assert_eq!(body["conn"]["runningApp"], json!({"id":"game"}));
    registration.respond(200, registration_response());
    state(&client, |s| s.connection == Connection::Ready).await;
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn game_presence_refusal_does_not_retry_or_replace_manual_status() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client
        .start_game_presence(crate::friends::Activity {
            id: "game".into(),
            name: None,
        })
        .unwrap();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let registration = fixture.registration().await;
    let body: Value = serde_json::from_str(&registration.request.body).unwrap();
    assert_eq!(body["status"], "online");
    assert!(body.get("statusSettings").is_none());
    registration.respond(403, json!({}));
    state(&client, |s| s.connection == Connection::Forbidden).await;
    peer.closed().await;
    assert!(fixture.http.try_recv().is_err());
}

#[tokio::test]
async fn game_presence_can_stop_during_registration() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client
        .start_game_presence(crate::friends::Activity {
            id: "game".into(),
            name: None,
        })
        .unwrap();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let _pending = fixture.registration().await;
    client.stop_friends();
    peer.closed().await;
    assert_eq!(client.friends().connection, Connection::Stopped);
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(SAFETY_BOUND, future)
        .await
        .expect("loopback fixture did not reach the expected barrier")
}

async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|context| {
        assert!(future.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[derive(Clone, Debug)]
struct HttpRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: String,
}

struct PendingHttp {
    request: HttpRequest,
    response: oneshot::Sender<(u16, Value)>,
}

impl PendingHttp {
    fn respond(self, status: u16, body: Value) {
        assert!(self.response.send((status, body)).is_ok());
    }
}

struct Peer {
    socket: WebSocketStream<TcpStream>,
    authorization: String,
    protocol: Option<String>,
}

impl Peer {
    async fn text(&mut self) -> String {
        let message = bounded(self.socket.next())
            .await
            .expect("client socket closed")
            .expect("invalid client WebSocket message");
        match message {
            Message::Text(value) => value.to_string(),
            other => panic!("expected STOMP text, received {other:?}"),
        }
    }

    async fn subscribe(&mut self) {
        self.subscribe_with_token("fixture-access-token").await;
    }

    async fn subscribe_with_token(&mut self, token: &str) {
        self.subscribe_with_heartbeat(token, "0,0").await;
    }

    async fn subscribe_with_heartbeat(&mut self, token: &str, server_heartbeat: &str) {
        assert_eq!(self.authorization, token);
        assert_eq!(self.protocol, None);
        // Keep standard STOMP negotiation and the Epic gateway's heartbeat alias.
        assert_eq!(
            self.text().await,
            "CONNECT\naccept-version:1.2\nhost:127.0.0.1\nheart-beat:30000,0\nheartbeat:30000,0\n\n\0"
        );
        self.socket
            .send(Message::text(format!(
                "CONNECTED\nversion:1.2\nheart-beat:{server_heartbeat}\n\n\0"
            )))
            .await
            .unwrap();
        let subscribe = self.text().await;
        assert!(subscribe.starts_with("SUBSCRIBE\nid:sub-0\ndestination:"));
        assert!(subscribe.ends_with("\nack:auto\nreceipt:friends-ready\n\n\0"));
        let destination = subscribe
            .lines()
            .find_map(|line| line.strip_prefix("destination:"))
            .unwrap();
        let prefix = destination.strip_suffix("/account/account-a").unwrap();
        assert_eq!(prefix.len(), 32);
        assert!(prefix.bytes().all(|byte| byte.is_ascii_hexdigit()));
        self.socket
            .send(Message::text("RECEIPT\nreceipt-id:friends-ready\n\n\0"))
            .await
            .unwrap();
        self.event(r#"{"type":"core.connect.v1.connected","connectionId":"fixture-connection"}"#)
            .await;
    }

    async fn event(&mut self, json: &str) {
        // Handwritten wire envelope; no production STOMP encoder participates.
        let wire = format!(
            "MESSAGE\nsubscription:sub-0\nmessage-id:fixture\ndestination:fixture/account/account-a\ncontent-type:application/json\n\n{json}\0"
        );
        self.socket.send(Message::text(wire)).await.unwrap();
    }

    async fn barrier(&mut self) {
        self.socket
            .send(Message::Ping(b"barrier".to_vec().into()))
            .await
            .unwrap();
        bounded(async {
            loop {
                let message = self.socket.next().await.unwrap().unwrap();
                match message {
                    Message::Ping(_) => self.socket.flush().await.unwrap(),
                    Message::Text(text) if text == "\n" => {}
                    Message::Pong(value) if value.as_ref() == b"barrier" => break,
                    other => panic!("unexpected message before fixture barrier: {other:?}"),
                }
            }
        })
        .await;
    }

    async fn closed(&mut self) {
        match bounded(self.socket.next()).await {
            None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
            other => panic!("observer should release its socket: {other:?}"),
        }
    }
}

struct Fixture {
    api: Api,
    http: mpsc::UnboundedReceiver<PendingHttp>,
    peers: mpsc::UnboundedReceiver<Peer>,
    log: Arc<Mutex<Vec<HttpRequest>>>,
    servers: Vec<JoinHandle<()>>,
}

impl Fixture {
    async fn new() -> Self {
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_base = format!("http://{}", http_listener.local_addr().unwrap());
        let ws_base = format!("ws://{}/", ws_listener.local_addr().unwrap());
        let (requests, http) = mpsc::unbounded_channel();
        let (connections, peers) = mpsc::unbounded_channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let captured = log.clone();
        let http_server = tokio::spawn(async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    connection = http_listener.accept() => {
                        let (socket, _) = connection.unwrap();
                        children.spawn(serve_http(socket, requests.clone(), captured.clone()));
                    }
                    done = children.join_next(), if !children.is_empty() => { done.unwrap().unwrap(); }
                }
            }
        });
        let ws_server = tokio::spawn(async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    connection = ws_listener.accept() => {
                        let (socket, _) = connection.unwrap();
                        let connections = connections.clone();
                        children.spawn(async move {
                            let upgrade = Arc::new(Mutex::new(None));
                            let headers = upgrade.clone();
                            // Tungstenite fixes the callback error type to an HTTP response.
                            #[allow(clippy::result_large_err)]
                            let socket = accept_hdr_async(socket, move |request: &Request, response: Response| {
                                *headers.lock().unwrap() = Some((
                                    request.headers()["authorization"].to_str().unwrap().to_owned(),
                                    request.headers().get("sec-websocket-protocol").map(|value| value.to_str().unwrap().to_owned()),
                                ));
                                Ok(response)
                            }).await.unwrap();
                            let (authorization, protocol) = upgrade.lock().unwrap().take().unwrap();
                            let _ = connections.send(Peer { socket, authorization, protocol });
                        });
                    }
                    done = children.join_next(), if !children.is_empty() => { done.unwrap().unwrap(); }
                }
            }
        });
        let mut api = Api::new().unwrap();
        api.account = http_base.clone();
        api.library = http_base.clone();
        api.launcher = http_base.clone();
        api.catalog = http_base.clone();
        api.ecommerce = http_base.clone();
        api.friends = http_base.clone();
        api.presence = http_base;
        api.connect = ws_base;
        Self {
            api,
            http,
            peers,
            log,
            servers: vec![http_server, ws_server],
        }
    }

    fn client(&self) -> Arc<Client> {
        self.client_with_expiry(Instant::now() + Duration::from_secs(3600))
    }

    fn client_with_expiry(&self, expires_at: Instant) -> Arc<Client> {
        Arc::new(Client::from_session(
            self.api.clone(),
            Session {
                credentials: Credentials {
                    account_id: "account-a".into(),
                    display_name: "Fixture Owner".into(),
                    refresh_token: "fixture-refresh-token".into(),
                },
                access_token: "fixture-access-token".into(),
                expires_at,
            },
        ))
    }

    async fn peer(&mut self) -> Peer {
        bounded(self.peers.recv()).await.unwrap()
    }

    async fn bootstrap(&mut self) -> (PendingHttp, PendingHttp) {
        self.bootstrap_with_token("fixture-access-token").await
    }

    async fn bootstrap_with_token(&mut self, token: &str) -> (PendingHttp, PendingHttp) {
        let first = bounded(self.http.recv()).await.unwrap();
        let second = bounded(self.http.recv()).await.unwrap();
        let (summary, presence) = if first.request.target == SUMMARY_PATH {
            (first, second)
        } else {
            (second, first)
        };
        assert_eq!(summary.request.target, SUMMARY_PATH);
        assert_eq!(presence.request.target, REGISTRATION_PATH);
        for request in [&summary.request, &presence.request] {
            assert_eq!(
                request.method,
                if request.target == SUMMARY_PATH {
                    "GET"
                } else {
                    "POST"
                }
            );
            assert_eq!(request.headers["authorization"], format!("Bearer {token}"));
        }
        (summary, presence)
    }

    async fn metadata(&mut self) -> PendingHttp {
        let request = bounded(self.http.recv()).await.unwrap();
        assert_eq!(request.request.target, SUMMARY_PATH);
        assert_eq!(request.request.method, "GET");
        request
    }

    async fn registration(&mut self) -> PendingHttp {
        let request = bounded(self.http.recv()).await.unwrap();
        assert_eq!(request.request.target, REGISTRATION_PATH);
        assert_eq!(request.request.method, "POST");
        assert_eq!(request.request.headers["x-http-method-override"], "PATCH");
        request
    }

    async fn renewal(&mut self) -> PendingHttp {
        let request = bounded(self.http.recv()).await.unwrap();
        assert_eq!(request.request.method, "POST");
        assert_eq!(request.request.target, "/account/api/oauth/token");
        assert!(request.request.headers["authorization"].starts_with("Basic "));
        assert!(request.request.body.contains("grant_type=refresh_token"));
        assert!(
            request
                .request
                .body
                .contains("refresh_token=fixture-refresh-token")
        );
        request
    }

    async fn profiles(&mut self, account_ids: &[&str]) -> PendingHttp {
        let request = bounded(self.http.recv()).await.unwrap();
        assert_eq!(request.request.method, "GET");
        assert_eq!(
            request.request.headers["authorization"],
            "Bearer fixture-access-token"
        );
        let url =
            reqwest::Url::parse(&format!("http://fixture{}", request.request.target)).unwrap();
        assert_eq!(url.path(), PROFILES_PATH);
        let mut queried: Vec<_> = url
            .query_pairs()
            .map(|(key, value)| {
                assert_eq!(key, "accountId");
                value.into_owned()
            })
            .collect();
        queried.sort();
        let mut expected = account_ids.to_vec();
        expected.sort();
        assert_eq!(queried, expected);
        request
    }

    fn assert_request_counts(&mut self, bootstrap_pairs: usize, profile_reads: usize) {
        assert!(
            self.http.try_recv().is_err(),
            "unexpected queued HTTP request"
        );
        let log = self.log.lock().unwrap();
        assert_eq!(log.len(), bootstrap_pairs + 1 + profile_reads);
        assert_eq!(
            log.iter()
                .filter(|request| request.target == SUMMARY_PATH)
                .count(),
            bootstrap_pairs
        );
        assert_eq!(
            log.iter()
                .filter(|request| request.target == REGISTRATION_PATH)
                .count(),
            1
        );
        assert_eq!(
            log.iter()
                .filter(|request| request.target.starts_with(PROFILES_PATH))
                .count(),
            profile_reads
        );
    }

    async fn ready(&mut self) -> (Arc<Client>, Peer, Snapshot) {
        let client = self.client();
        client.start_friends();
        let mut peer = self.peer().await;
        peer.subscribe().await;
        let (summary_request, presence_request) = self.bootstrap().await;
        summary_request.respond(200, summary());
        presence_request.respond(200, registration_response());
        let ready = state(&client, |s| s.connection == Connection::Ready).await;
        (client, peer, ready)
    }

    fn assert_only_bootstrap_requests(&mut self, count: usize) {
        assert!(
            self.http.try_recv().is_err(),
            "unexpected queued HTTP request"
        );
        let log = self.log.lock().unwrap();
        assert_eq!(log.len(), count);
        assert!(log.iter().all(|request| (request.method == "GET"
            && request.target == SUMMARY_PATH)
            || (request.method == "POST" && request.target == REGISTRATION_PATH)));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

async fn serve_http(
    mut socket: TcpStream,
    requests: mpsc::UnboundedSender<PendingHttp>,
    log: Arc<Mutex<Vec<HttpRequest>>>,
) {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let request = loop {
        let length = socket.read(&mut chunk).await.unwrap();
        if length == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..length]);
        assert!(bytes.len() <= 64 * 1024);
        let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
        let mut lines = headers.lines();
        let mut start = lines.next().unwrap().split_whitespace();
        let method = start.next().unwrap().to_owned();
        let target = start.next().unwrap().to_owned();
        let headers: BTreeMap<_, _> = lines
            .map(|line| {
                let (key, value) = line.split_once(':').unwrap();
                (key.to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        let body_length = headers
            .get("content-length")
            .map_or(0, |value| value.parse::<usize>().unwrap());
        if bytes.len() >= end + 4 + body_length {
            break HttpRequest {
                method,
                target,
                headers,
                body: std::str::from_utf8(&bytes[end + 4..end + 4 + body_length])
                    .unwrap()
                    .to_owned(),
            };
        }
    };
    log.lock().unwrap().push(request.clone());
    let (response, receive) = oneshot::channel();
    if requests.send(PendingHttp { request, response }).is_err() {
        return;
    }
    let Ok((status, body)) = receive.await else {
        return;
    };
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

fn summary() -> Value {
    json!({"friends":[{"accountId":"alice","displayName":"Alice"}],"blocklist":[]})
}
fn registration_response() -> Value {
    json!({"own":{"accountId":"account-a","status":"online","perNs":[]}})
}

fn renewed_token() -> Value {
    json!({"access_token":"fixture-access-new", "refresh_token":"fixture-refresh-new",
        "account_id":"account-a", "displayName":"Fixture Owner", "expires_in":3600})
}

async fn advance_time(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

async fn state(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    bounded(async {
        let mut snapshot = client.friends();
        while !predicate(&snapshot) {
            snapshot = client.next_friends(snapshot.revision).await;
        }
        snapshot
    })
    .await
}

#[tokio::test]
async fn registers_connection_and_receives_initial_presence_without_a_snapshot_read() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    peer.event(r#"{"type":"core.connect.v1.connected","connectionId":"fixture-connection"}"#)
        .await;
    let first = bounded(fixture.http.recv()).await.unwrap();
    let second = bounded(fixture.http.recv()).await.unwrap();
    let (summary_request, registration) = if first.request.target == SUMMARY_PATH {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(registration.request.method, "POST");
    assert_eq!(
        registration.request.target,
        "/epic/presence/internal/v1/_/account-a/presence/fixture-connection"
    );
    assert_eq!(
        registration.request.headers["x-http-method-override"],
        "PATCH"
    );
    assert_eq!(registration.request.headers["x-epic-agent"], "epic-social");
    assert_eq!(
        serde_json::from_str::<Value>(&registration.request.body).unwrap(),
        json!({"status":"online","conn":{"props":{}}})
    );
    registration.respond(
        200,
        json!({"own":{"accountId":"account-a","status":"away","perNs":[]}}),
    );
    peer.event(AWAY).await;
    peer.barrier().await;
    summary_request.respond(200, json!({"friends":[{"accountId":"alice","displayName":"Alice"},{"accountId":"bob","displayName":"Bob"}]}));
    let ready = state(&client, |s| s.connection == Connection::Ready).await;
    assert_eq!(ready.friends[0].presence, Presence::Away);
    assert_eq!(ready.friends[1].presence, Presence::Unknown);
    assert_eq!(ready.message, None);
    peer.event(ONLINE).await;
    let changed = state(&client, |s| s.friends[0].presence == Presence::Online).await;
    assert_eq!(changed.connection, Connection::Ready);
    peer.barrier().await;
    assert!(fixture.http.try_recv().is_err());
    assert_eq!(fixture.log.lock().unwrap().len(), 2);
    client.stop_friends();
}

#[tokio::test]
async fn bootstraps_real_client_and_replays_presence_received_during_http() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    client.start_friends(); // An existing observer is not duplicated.
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    peer.event(ONLINE).await;
    peer.event(PLAYING).await;
    peer.barrier().await;
    assert_eq!(client.friends().connection, Connection::Loading);
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let ready = state(&client, |s| s.connection == Connection::Ready).await;
    assert_eq!(ready.account_id, "account-a");
    assert_eq!(ready.friends.len(), 1);
    assert_eq!(ready.friends[0].display_name.as_deref(), Some("Alice"));
    assert_eq!(ready.friends[0].presence, Presence::Online);
    assert_eq!(
        ready.friends[0].activity.as_ref().unwrap().name.as_deref(),
        Some("Fixture Game")
    );
    fixture.assert_only_bootstrap_requests(2);
    assert!(fixture.peers.try_recv().is_err());
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn spontaneous_socket_events_wake_watchers_and_slow_watchers_receive_latest_state() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, ready) = fixture.ready().await;
    let waiting = client.next_friends(ready.revision);
    tokio::pin!(waiting);
    assert_pending(waiting.as_mut()).await;
    peer.event(AWAY).await;
    let away = bounded(waiting).await;
    assert!(away.revision > ready.revision);
    assert_eq!(away.friends[0].presence, Presence::Away);

    peer.event(ONLINE).await;
    peer.barrier().await;
    peer.event(PLAYING).await;
    peer.barrier().await;
    let latest = bounded(client.next_friends(away.revision)).await;
    assert_eq!(latest.revision, client.friends().revision);
    assert!(latest.revision >= away.revision + 2);
    assert_eq!(latest.friends[0].presence, Presence::Online);
    assert_eq!(latest.friends[0].activity.as_ref().unwrap().id, "game-a");
    fixture.assert_only_bootstrap_requests(2);
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn removal_and_late_presence_cannot_resurrect_a_friend_from_delayed_http() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, _) = fixture.ready().await;
    peer.event(REMOVED).await;
    let summary_request = fixture.metadata().await;
    peer.event(ONLINE).await;
    peer.barrier().await;
    assert!(client.friends().friends.is_empty());
    // Bob makes completion of the replacement snapshot observable. Alice's entry
    // is an intentionally stale HTTP result that must lose to the socket removal.
    summary_request.respond(
        200,
        json!({"friends":[
        {"accountId":"alice","displayName":"Alice"},
        {"accountId":"bob","displayName":"Bob"}
    ],"blocklist":[]}),
    );
    let refreshed = state(&client, |s| s.friends.iter().any(|f| f.account_id == "bob")).await;
    assert_eq!(refreshed.connection, Connection::Ready);
    assert_eq!(refreshed.friends.len(), 1);
    assert_eq!(refreshed.friends[0].account_id, "bob");
    peer.event(PLAYING).await;
    peer.barrier().await;
    assert_eq!(client.friends(), refreshed);
    fixture.assert_only_bootstrap_requests(3);
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn five_idle_minutes_do_not_poll_http() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, ready) = fixture.ready().await;
    peer.barrier().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    peer.barrier().await;
    assert_eq!(client.friends(), ready);
    fixture.assert_only_bootstrap_requests(2);
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn stopping_wakes_waiters_and_restarting_observers_preserves_the_oauth_session() {
    let mut fixture = Fixture::new().await;
    let (client, mut first_peer, ready) = fixture.ready().await;
    let waiting = client.next_friends(ready.revision);
    let credentials_waiting = client.next_credentials_revision(1);
    tokio::pin!(waiting, credentials_waiting);
    assert_pending(waiting.as_mut()).await;
    assert_pending(credentials_waiting.as_mut()).await;
    client.stop_friends();
    let stopped = bounded(waiting).await;
    assert_eq!(stopped.connection, Connection::Stopped);
    assert!(stopped.friends.is_empty());
    assert!(stopped.revision > ready.revision);
    first_peer.closed().await;
    assert_pending(credentials_waiting.as_mut()).await;
    assert_eq!(
        client.prepare().await.unwrap().refresh_token,
        "fixture-refresh-token"
    );
    assert!(client.progress().is_none());

    client.reconnect_friends();
    let mut second_peer = fixture.peer().await;
    second_peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let restarted = state(&client, |s| s.connection == Connection::Ready).await;
    assert!(restarted.revision > stopped.revision);
    assert_eq!(restarted.friends.len(), 1);
    let waiting = client.next_friends(restarted.revision);
    tokio::pin!(waiting);
    assert_pending(waiting.as_mut()).await;
    client.shutdown_observers();
    assert_eq!(bounded(waiting).await.connection, Connection::Stopped);
    assert_eq!(bounded(credentials_waiting).await, None);
    second_peer.closed().await;
    assert_eq!(client.credentials().await.unwrap().account_id, "account-a");
    assert_eq!(
        client.prepare().await.unwrap().refresh_token,
        "fixture-refresh-token"
    );
    fixture.assert_only_bootstrap_requests(4);
}

#[tokio::test]
async fn forbidden_social_access_does_not_invalidate_launcher_authentication() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    presence_request.respond(200, registration_response());
    summary_request.respond(403, json!({"errorMessage":"fixture permission missing"}));
    let forbidden = state(&client, |s| s.connection == Connection::Forbidden).await;
    assert_eq!(
        forbidden.message.as_deref(),
        Some("backend.epic.friends.forbidden")
    );
    assert!(forbidden.friends.is_empty());
    peer.closed().await;
    assert_eq!(
        client.prepare().await.unwrap().refresh_token,
        "fixture-refresh-token"
    );
    fixture.assert_only_bootstrap_requests(2);
    client.stop_friends();
}

#[tokio::test]
async fn social_refresh_notifies_credentials_before_http_and_serializes_concurrent_prepare() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client_with_expiry(Instant::now() - Duration::from_secs(1));
    let initial = client.friends();
    let credentials_waiting = client.next_credentials_revision(1);
    let friends_waiting = client.next_friends(initial.revision);
    tokio::pin!(credentials_waiting, friends_waiting);
    assert_pending(credentials_waiting.as_mut()).await;
    assert_pending(friends_waiting.as_mut()).await;
    client.start_friends();
    let renewal = fixture.renewal().await;
    // The social request holds the shared token refresh lock. A library prepare
    // started before its response must join that renewal, not create another POST.
    let prepare = client.prepare();
    tokio::pin!(prepare);
    assert_pending(prepare.as_mut()).await;
    assert_eq!(fixture.log.lock().unwrap().len(), 1);
    renewal.respond(200, renewed_token());
    assert_eq!(bounded(credentials_waiting).await, Some(2));
    assert_eq!(
        bounded(prepare).await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    assert_eq!(client.friends(), initial);
    assert_pending(friends_waiting.as_mut()).await;

    let mut peer = fixture.peer().await;
    peer.subscribe_with_token("fixture-access-new").await;
    let (summary_request, presence_request) =
        fixture.bootstrap_with_token("fixture-access-new").await;
    assert_pending(friends_waiting.as_mut()).await;
    presence_request.respond(200, registration_response());
    summary_request.respond(
        403,
        json!({"errorMessage":"fixture social permission denied"}),
    );
    let failure = bounded(friends_waiting).await;
    assert_eq!(failure.connection, Connection::Forbidden);
    assert!(failure.friends.is_empty());
    peer.closed().await;
    assert_eq!(
        client.prepare().await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    let log = fixture.log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log.iter()
            .filter(|r| r.target == "/account/api/oauth/token")
            .count(),
        1
    );
    drop(log);
    client.stop_friends();
}

#[tokio::test]
async fn social_401_renews_once_and_repeated_rejection_is_not_oauth_expiration() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut first_peer = fixture.peer().await;
    first_peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    presence_request.respond(200, registration_response());
    summary_request.respond(401, json!({"errorMessage":"fixture old token rejected"}));
    first_peer.closed().await;
    fixture.renewal().await.respond(200, renewed_token());
    assert_eq!(bounded(client.next_credentials_revision(1)).await, Some(2));
    let mut second_peer = fixture.peer().await;
    second_peer.subscribe_with_token("fixture-access-new").await;
    let (summary_request, presence_request) =
        fixture.bootstrap_with_token("fixture-access-new").await;
    presence_request.respond(200, registration_response());
    summary_request.respond(
        401,
        json!({"errorMessage":"fixture permission denied with new token"}),
    );
    let failure = state(&client, |s| s.connection == Connection::Forbidden).await;
    assert_eq!(
        failure.message.as_deref(),
        Some("backend.epic.friends.forbidden")
    );
    second_peer.closed().await;
    assert_eq!(
        client.prepare().await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    assert!(fixture.http.try_recv().is_err());
    assert!(fixture.peers.try_recv().is_err());
    let log = fixture.log.lock().unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(
        log.iter()
            .filter(|r| r.target == "/account/api/oauth/token")
            .count(),
        1
    );
    drop(log);
    client.stop_friends();
}

#[tokio::test]
async fn rejected_refresh_grant_reports_authentication_required() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    presence_request.respond(200, registration_response());
    summary_request.respond(401, json!({"errorMessage":"fixture expired access token"}));
    peer.closed().await;
    fixture
        .renewal()
        .await
        .respond(400, json!({"error":"invalid_grant"}));
    let failure = state(&client, |s| {
        s.connection == Connection::AuthenticationRequired
    })
    .await;
    assert_eq!(
        failure.message.as_deref(),
        Some("backend.epic.unauthorized")
    );
    assert!(fixture.peers.try_recv().is_err());
    assert_eq!(fixture.log.lock().unwrap().len(), 3);
    client.stop_friends();
}

#[tokio::test]
async fn malformed_stomp_retries_are_bounded_and_explicit_reconnect_resynchronizes() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, ready) = fixture.ready().await;
    peer.socket
        .send(Message::text("MESSAGE\ninvalid-header\n\n\0"))
        .await
        .unwrap();
    for delay in [1, 2, 4, 8] {
        let interrupted = state(&client, |s| s.connection == Connection::Interrupted).await;
        assert_eq!(interrupted.friends, ready.friends);
        peer.closed().await;
        advance_time(Duration::from_secs(delay)).await;
        peer = fixture.peer().await;
        assert!(peer.text().await.starts_with("CONNECT\n"));
        // Wrong-version CONNECTED is also a fatal STOMP setup failure.
        peer.socket
            .send(Message::text("CONNECTED\nversion:1.1\n\n\0"))
            .await
            .unwrap();
    }
    let failed = state(&client, |s| s.connection == Connection::Failed).await;
    assert_eq!(failed.friends, ready.friends);
    assert_eq!(
        failed.message.as_deref(),
        Some("backend.epic.friends.protocol")
    );
    peer.closed().await;
    advance_time(Duration::from_secs(300)).await;
    assert!(fixture.peers.try_recv().is_err());
    assert_eq!(client.friends(), failed);
    fixture.assert_only_bootstrap_requests(2);

    client.reconnect_friends();
    let mut recovered_peer = fixture.peer().await;
    recovered_peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, json!({"friends":[], "blocklist":[]}));
    presence_request.respond(200, registration_response());
    let recovered = state(&client, |s| s.connection == Connection::Ready).await;
    assert!(recovered.friends.is_empty());
    assert!(recovered.revision > failed.revision);
    fixture.assert_only_bootstrap_requests(4);
    client.stop_friends();
    recovered_peer.closed().await;
}

#[tokio::test]
async fn bootstrap_event_buffer_overflow_reconnects_and_reads_a_fresh_snapshot() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let initial_requests = fixture.bootstrap().await;
    let frame = format!("MESSAGE\nsubscription:sub-0\ncontent-type:application/json\n\n{ONLINE}\0");
    peer.socket
        .send(Message::text(frame.repeat(513)))
        .await
        .unwrap();
    let interrupted = state(&client, |s| s.connection == Connection::Interrupted).await;
    assert!(interrupted.friends.is_empty());
    peer.closed().await;
    drop(initial_requests);
    advance_time(Duration::from_secs(1)).await;
    let mut replacement = fixture.peer().await;
    replacement.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let ready = state(&client, |s| s.connection == Connection::Ready).await;
    assert_eq!(ready.friends.len(), 1);
    // Events buffered on the old connection cannot supply the new initial state.
    assert_eq!(ready.friends[0].presence, Presence::Unknown);
    fixture.assert_only_bootstrap_requests(4);
    client.stop_friends();
    replacement.closed().await;
}

#[tokio::test]
async fn restarting_social_during_oauth_response_preserves_rotation_and_uses_one_refresh() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client_with_expiry(Instant::now() - Duration::from_secs(1));
    let credentials_waiting = client.next_credentials_revision(1);
    tokio::pin!(credentials_waiting);
    assert_pending(credentials_waiting.as_mut()).await;
    client.start_friends();
    let renewal = fixture.renewal().await;
    // The OAuth server has accepted the old token, but its rotated credentials
    // have not reached the client when a system wake restarts the social task.
    client.reconnect_friends();
    renewal.respond(200, renewed_token());
    let revision = bounded(async {
        tokio::select! {
            revision = credentials_waiting.as_mut() => revision,
            request = fixture.http.recv() => {
                panic!("observer restart duplicated the OAuth request: {}", request.unwrap().request.target);
            }
        }
    }).await;
    assert_eq!(revision, Some(2));
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "fixture-refresh-new"
    );
    let mut replacement = fixture.peer().await;
    replacement.subscribe_with_token("fixture-access-new").await;
    let (summary_request, presence_request) =
        fixture.bootstrap_with_token("fixture-access-new").await;
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let ready = state(&client, |s| s.connection == Connection::Ready).await;
    assert_eq!(ready.friends.len(), 1);
    replacement.barrier().await;
    {
        let log = fixture.log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(
            log.iter()
                .filter(|r| r.target == "/account/api/oauth/token")
                .count(),
            1
        );
    }
    assert!(fixture.peers.try_recv().is_err());
    client.stop_friends();
    replacement.closed().await;
}

#[tokio::test]
async fn relations_received_during_http_schedule_one_followup_for_names_and_new_friends() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, _) = fixture.ready().await;
    const UPDATED: &str =
        r#"{"type":"friends.v1.FRIEND_ENTRY_UPDATED","payload":{"from":"account-a","to":"alice"}}"#;
    const ADDED: &str =
        r#"{"type":"friends.v1.FRIEND_ENTRY_ADDED","payload":{"from":"account-a","to":"bob"}}"#;
    peer.event(UPDATED).await;
    let summary_request = fixture.metadata().await;
    peer.event(ADDED).await;
    peer.event(UPDATED).await;
    peer.barrier().await;
    summary_request.respond(200, summary());
    // This snapshot began before Bob's addition and Alice's latest update. A
    // replay alone cannot recover their names from ID-only relationship events.
    let followup_summary = fixture.metadata().await;
    followup_summary.respond(
        200,
        json!({"friends":[
        {"accountId":"alice","displayName":"Alice Updated"},
        {"accountId":"bob","displayName":"Bob"}
    ],"blocklist":[]}),
    );
    peer.event(r#"{"type":"presence.v1.UPDATE","payload":{"accountId":"bob","status":"online","perNs":[]}}"#).await;
    peer.barrier().await;
    let ready = state(&client, |s| {
        s.friends
            .iter()
            .any(|f| f.account_id == "alice" && f.display_name.as_deref() == Some("Alice Updated"))
            && s.friends
                .iter()
                .any(|f| f.account_id == "bob" && f.display_name.as_deref() == Some("Bob"))
    })
    .await;
    assert_eq!(ready.connection, Connection::Ready);
    assert_eq!(ready.friends.len(), 2);
    assert_eq!(ready.friends[1].presence, Presence::Online);
    peer.barrier().await;
    fixture.assert_only_bootstrap_requests(4);
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn negotiates_stomp_12_without_a_websocket_subprotocol_header() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    // Epic's observed HTTP 101 omits Sec-WebSocket-Protocol. The STOMP frames
    // independently negotiate and validate 1.2 after the WebSocket upgrade.
    assert_eq!(peer.protocol, None);
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let ready = state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
    assert_eq!(ready.friends[0].display_name.as_deref(), Some("Alice"));
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn server_heartbeat_offer_does_not_enable_an_unrequested_incoming_timeout() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe_with_heartbeat("fixture-access-token", "30000,0")
        .await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, summary());
    presence_request.respond(200, registration_response());
    let ready = state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
    peer.barrier().await;
    // The client advertises zero for incoming heartbeats. A server offer alone
    // must not create a timeout that tears down an otherwise idle connection.
    advance_time(Duration::from_secs(180)).await;
    peer.barrier().await;
    assert_eq!(client.friends(), ready);
    fixture.assert_only_bootstrap_requests(2);
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn forbidden_initial_presence_keeps_membership_and_live_events_without_polling() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    // The gateway accepts this launcher token while initialpresence can refuse
    // its scope. A failed presence request must not cancel accepted membership.
    presence_request.respond(403, json!({"errorCode":"fixture.presence.forbidden"}));
    summary_request.respond(200, summary());
    let partial = state(&client, |snapshot| {
        snapshot.connection == Connection::Forbidden
    })
    .await;
    assert_eq!(partial.friends.len(), 1);
    assert_eq!(partial.friends[0].account_id, "alice");
    assert_eq!(partial.friends[0].display_name.as_deref(), Some("Alice"));
    assert_eq!(partial.friends[0].presence, Presence::Unknown);
    assert_eq!(partial.friends[0].activity, None);
    assert_eq!(
        partial.message.as_deref(),
        Some("backend.epic.friends.presenceForbidden")
    );

    let waiting = client.next_friends(partial.revision);
    tokio::pin!(waiting);
    assert_pending(waiting.as_mut()).await;
    peer.event(PLAYING).await;
    let playing = bounded(waiting).await;
    assert!(playing.revision > partial.revision);
    assert_eq!(playing.connection, Connection::Forbidden);
    assert_eq!(playing.message, partial.message);
    assert_eq!(playing.friends.len(), 1);
    assert_eq!(playing.friends[0].presence, Presence::Online);
    let activity = playing.friends[0].activity.as_ref().unwrap();
    assert_eq!(activity.id, "game-a");
    assert_eq!(activity.name.as_deref(), Some("Fixture Game"));

    advance_time(Duration::from_secs(300)).await;
    peer.barrier().await;
    assert_eq!(client.friends(), playing);
    assert!(fixture.peers.try_recv().is_err());
    fixture.assert_only_bootstrap_requests(2);
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "fixture-refresh-token"
    );
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn forbidden_presence_reconciliation_preserves_live_activity_while_renaming_friend() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, summary());
    presence_request.respond(403, json!({"errorCode":"fixture.presence.forbidden"}));
    state(&client, |snapshot| {
        snapshot.connection == Connection::Forbidden
    })
    .await;
    peer.event(PLAYING).await;
    let playing = state(&client, |snapshot| snapshot.friends[0].activity.is_some()).await;

    peer.event(
        r#"{"type":"friends.v1.FRIEND_ENTRY_UPDATED","payload":{"from":"account-a","to":"alice"}}"#,
    )
    .await;
    let updated_summary = fixture.metadata().await;
    updated_summary.respond(
        200,
        json!({"friends":[{"accountId":"alice","displayName":"Alice Renamed"}],"blocklist":[]}),
    );
    let renamed = state(&client, |snapshot| {
        snapshot.friends[0].display_name.as_deref() == Some("Alice Renamed")
    })
    .await;
    assert_eq!(renamed.connection, Connection::Forbidden);
    assert_eq!(renamed.message, playing.message);
    assert_eq!(renamed.friends[0].presence, Presence::Online);
    assert_eq!(renamed.friends[0].activity, playing.friends[0].activity);
    peer.barrier().await;
    fixture.assert_only_bootstrap_requests(3);
    assert!(fixture.peers.try_recv().is_err());
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn missing_websocket_pong_disconnects_and_exhausts_bounded_retries() {
    let mut fixture = Fixture::new().await;
    let (client, mut peer, initial) = fixture.ready().await;
    for attempt in 0..5 {
        advance_time(Duration::from_secs(30)).await;
        assert!(matches!(
            bounded(peer.socket.next()).await,
            Some(Ok(Message::Ping(_)))
        ));
        // Reading Ping queues tungstenite's automatic Pong. Keep it unflushed
        // until the client's watchdog expires, emulating an unresponsive peer.
        advance_time(Duration::from_secs(20)).await;
        let expected = if attempt == 4 {
            Connection::Failed
        } else {
            Connection::Interrupted
        };
        let disconnected = state(&client, |snapshot| snapshot.connection == expected).await;
        assert_eq!(disconnected.friends, initial.friends);
        assert_eq!(
            disconnected.message.as_deref(),
            Some("backend.epic.friends.network")
        );
        peer.closed().await;
        if attempt < 4 {
            advance_time(Duration::from_secs(1 << attempt)).await;
            peer = fixture.peer().await;
            peer.subscribe().await;
            let (summary_request, presence_request) = fixture.bootstrap().await;
            summary_request.respond(200, summary());
            presence_request.respond(200, registration_response());
            state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
        }
    }
    let failed = client.friends();
    advance_time(Duration::from_secs(300)).await;
    assert_eq!(client.friends(), failed);
    assert!(fixture.peers.try_recv().is_err());
    fixture.assert_only_bootstrap_requests(10);
    client.stop_friends();
}

#[tokio::test]
async fn account_profiles_hydrate_opaque_ids_and_replay_presence_received_while_loading() {
    const FRIEND_ID: &str = "4a19fbcd0123456789abcdef0123456789";
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(
        200,
        json!({"friends":[{"accountId":FRIEND_ID}],"blocklist":[]}),
    );
    presence_request.respond(200, registration_response());
    let profiles = fixture.profiles(&[FRIEND_ID]).await;
    peer.event(&PLAYING.replace("alice", FRIEND_ID)).await;
    peer.barrier().await;
    assert_eq!(client.friends().connection, Connection::Loading);
    profiles.respond(
        200,
        json!([{"id":FRIEND_ID,"displayName":"Hydrated Friend","externalAuths":{}}]),
    );
    let ready = state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
    assert_eq!(ready.friends.len(), 1);
    assert_eq!(ready.friends[0].account_id, FRIEND_ID);
    assert_eq!(
        ready.friends[0].display_name.as_deref(),
        Some("Hydrated Friend")
    );
    assert_eq!(ready.friends[0].presence, Presence::Online);
    let activity = ready.friends[0].activity.as_ref().unwrap();
    assert_eq!(activity.id, "game-a");
    assert_eq!(activity.name.as_deref(), Some("Fixture Game"));
    fixture.assert_request_counts(1, 1);
    assert!(fixture.peers.try_recv().is_err());
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn denied_account_profiles_preserve_unknown_rows_without_active_retries() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(
        200,
        json!({"friends":[{"accountId":"alice"}],"blocklist":[]}),
    );
    presence_request.respond(200, registration_response());
    fixture
        .profiles(&["alice"])
        .await
        .respond(403, json!({"errorCode":"fixture.profiles.forbidden"}));
    let ready = state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
    assert_eq!(ready.friends.len(), 1);
    assert_eq!(ready.friends[0].account_id, "alice");
    assert_eq!(ready.friends[0].display_name, None);
    assert_eq!(ready.friends[0].presence, Presence::Unknown);
    assert_eq!(ready.message, None);
    advance_time(Duration::from_secs(300)).await;
    peer.barrier().await;
    assert_eq!(client.friends(), ready);
    fixture.assert_request_counts(1, 1);
    assert!(fixture.peers.try_recv().is_err());
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "fixture-refresh-token"
    );
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn account_profile_cache_reuses_positive_and_negative_results_until_name_invalidation() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    let unnamed = json!({"friends":[{"accountId":"alice"},{"accountId":"bob"}],"blocklist":[]});
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(200, unnamed.clone());
    presence_request.respond(200, registration_response());
    fixture
        .profiles(&["alice", "bob"])
        .await
        .respond(200, json!([{"id":"alice","displayName":"Hydrated Alice"}]));
    let ready = state(&client, |snapshot| snapshot.connection == Connection::Ready).await;
    assert_eq!(
        ready.friends[0].display_name.as_deref(),
        Some("Hydrated Alice")
    );
    assert_eq!(ready.friends[1].display_name, None);

    peer.event(r#"{"type":"friends.v1.FRIEND_INVITE_ENTRY_ADDED","payload":{"from":"account-a","to":"carol"}}"#).await;
    let summary_request = fixture.metadata().await;
    summary_request.respond(200, unnamed.clone());
    peer.event(r#"{"type":"presence.v1.UPDATE","payload":{"accountId":"bob","status":"online","perNs":[]}}"#).await;
    peer.barrier().await;
    // Bob's presence change proves the reconciliation completed; neither the
    // named Alice nor the missing Bob profile should produce another request.
    let reused = state(&client, |snapshot| {
        snapshot.friends[1].presence == Presence::Online
    })
    .await;
    assert_eq!(
        reused.friends[0].display_name.as_deref(),
        Some("Hydrated Alice")
    );
    assert_eq!(reused.friends[1].display_name, None);
    fixture.assert_request_counts(2, 1);

    peer.event(r#"{"type":"friends.v1.FRIENDSHIP_ENTRY_UPDATE","payload":{"friendId":"alice"}}"#)
        .await;
    let summary_request = fixture.metadata().await;
    summary_request.respond(200, unnamed);
    fixture.profiles(&["alice", "bob"]).await.respond(
        200,
        json!([
            {"id":"alice","displayName":"Alice Renamed"},
            {"id":"bob","externalAuths":{"fixture":{"externalDisplayName":"Hydrated Bob"}}}
        ]),
    );
    let updated = state(&client, |snapshot| {
        snapshot.friends[0].display_name.as_deref() == Some("Alice Renamed")
    })
    .await;
    assert_eq!(
        updated.friends[1].display_name.as_deref(),
        Some("Hydrated Bob")
    );
    fixture.assert_request_counts(3, 2);
    assert!(fixture.peers.try_recv().is_err());
    client.stop_friends();
    peer.closed().await;
}

#[tokio::test]
async fn deletion_during_account_profile_loading_cannot_resurrect_from_metadata_or_presence() {
    let mut fixture = Fixture::new().await;
    let client = fixture.client();
    client.start_friends();
    let mut peer = fixture.peer().await;
    peer.subscribe().await;
    let (summary_request, presence_request) = fixture.bootstrap().await;
    summary_request.respond(
        200,
        json!({"friends":[{"accountId":"alice"}],"blocklist":[]}),
    );
    presence_request.respond(200, registration_response());
    let profiles = fixture.profiles(&["alice"]).await;
    peer.event(REMOVED).await;
    peer.event(PLAYING).await;
    peer.barrier().await;
    profiles.respond(200, json!([{"id":"alice","displayName":"Deleted Alice"}]));
    let followup_summary = fixture.metadata().await;
    // Even an older follow-up summary can still mention the removed ID.
    followup_summary.respond(
        200,
        json!({"friends":[
        {"accountId":"alice"}, {"accountId":"bob","displayName":"Bob"}
    ],"blocklist":[]}),
    );
    let ready = state(&client, |snapshot| {
        snapshot
            .friends
            .iter()
            .any(|friend| friend.account_id == "bob")
    })
    .await;
    assert_eq!(ready.connection, Connection::Ready);
    assert_eq!(ready.friends.len(), 1);
    assert_eq!(ready.friends[0].account_id, "bob");
    peer.event(PLAYING).await;
    peer.barrier().await;
    assert_eq!(client.friends(), ready);
    fixture.assert_request_counts(2, 1);
    client.stop_friends();
    peer.closed().await;
}
