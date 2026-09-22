//! One cancellable launcher WebSocket per Epic session. HTTP runs only at bootstrap,
//! after a relationship event, or when recovering a connection/buffer overflow.
use crate::{
    EpicError, auth,
    client::SocialAuth,
    friends::{Activity, Connection, Roster, Snapshot},
    stomp,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{self, Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

const MAX_BUFFERED_EVENTS: usize = 512;
const MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct SocialService {
    state: watch::Sender<Snapshot>,
    epoch: Arc<AtomicU64>,
    task: Mutex<Option<JoinHandle<()>>>,
}
impl SocialService {
    pub(crate) fn new(account: String) -> Self {
        Self {
            state: watch::channel(Snapshot::new(account)).0,
            epoch: Arc::new(AtomicU64::new(0)),
            task: Mutex::new(None),
        }
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }
    pub(crate) async fn next(&self, revision: u64) -> Snapshot {
        forge_core::watch::next_matching(self.state.subscribe(), |snapshot| {
            snapshot.revision != revision || snapshot.connection == Connection::Stopped
        })
        .await
        .unwrap_or_else(|last| last)
    }
    pub(crate) fn start(&self, auth: SocialAuth, force: bool, activity: Option<Activity>) {
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        if !force && task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        if let Some(previous) = task.take() {
            previous.abort();
        }
        let generation = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let publish = Publisher {
            state: self.state.clone(),
            epoch: self.epoch.clone(),
            generation,
        };
        publish.set(Connection::Loading, None, None);
        *task = Some(tokio::spawn(run(auth, publish, activity)));
    }
    pub(crate) fn stop(&self) {
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(task) = task.take() {
            task.abort();
        }
        self.state.send_modify(|s| {
            s.revision = s.revision.wrapping_add(1);
            s.connection = Connection::Stopped;
            s.friends.clear();
            s.message = None;
        });
    }
}
impl Drop for SocialService {
    fn drop(&mut self) {
        self.stop();
    }
}
struct Publisher {
    state: watch::Sender<Snapshot>,
    epoch: Arc<AtomicU64>,
    generation: u64,
}
impl Publisher {
    fn set(
        &self,
        connection: Connection,
        friends: Option<Vec<crate::friends::Friend>>,
        message: Option<&str>,
    ) {
        self.state.send_if_modified(|s| {
            if self.epoch.load(Ordering::SeqCst) != self.generation {
                return false;
            }
            let rows = friends.as_ref().unwrap_or(&s.friends);
            if s.connection == connection && rows == &s.friends && s.message.as_deref() == message {
                return false;
            }
            if let Some(friends) = friends {
                s.friends = friends;
            }
            s.connection = connection;
            s.message = message.map(str::to_owned);
            s.revision = s.revision.wrapping_add(1);
            true
        });
    }
}
#[derive(Debug)]
enum Failure {
    Network,
    Protocol,
    Forbidden,
    Expired,
    TokenRejected,
    RateLimited(Duration),
    Renew,
}
impl From<EpicError> for Failure {
    fn from(value: EpicError) -> Self {
        match value {
            EpicError::Unauthorized => Self::Expired,
            EpicError::Network => Self::Network,
            _ => Self::Protocol,
        }
    }
}
impl Failure {
    fn message(&self) -> &'static str {
        match self {
            Self::Network => "backend.epic.friends.network",
            Self::Protocol => "backend.epic.friends.protocol",
            Self::Forbidden | Self::TokenRejected => "backend.epic.friends.forbidden",
            Self::Expired => "backend.epic.unauthorized",
            Self::RateLimited(_) => "backend.epic.rate_limited",
            Self::Renew => "backend.epic.friends.network",
        }
    }
}
fn http_status(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Result<(), Failure> {
    match status.as_u16() {
        200..=299 => Ok(()),
        401 => Err(Failure::TokenRejected),
        403 => Err(Failure::Forbidden),
        429 => {
            // Do not retry earlier than Retry-After. Unsupported date syntax stops
            // the automatic cycle instead of guessing an earlier retry time.
            let wait = headers
                .get("retry-after")
                .map(|v| {
                    v.to_str()
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(86400)
                })
                .unwrap_or(60);
            Err(Failure::RateLimited(Duration::from_secs(wait.max(1))))
        }
        500..=599 => Err(Failure::Network),
        _ => Err(Failure::Protocol),
    }
}
async fn run(auth: SocialAuth, publish: Publisher, activity: Option<Activity>) {
    let account = publish.state.borrow().account_id.clone();
    let mut attempts = 0u32;
    let mut rejected = None;
    // The detached game supervisor has no user action to reconnect after an
    // outage. It keeps network recovery alive until the game stops or access is refused.
    let supervised = activity.is_some();
    loop {
        let mut ready_since = None;
        let (error, token) = match auth.access_token(rejected.as_deref()).await {
            Ok(token) => {
                let result = connection(
                    &auth,
                    &token,
                    &account,
                    &publish,
                    &mut ready_since,
                    activity.as_ref(),
                )
                .await;
                (result.err().unwrap_or(Failure::Network), Some(token))
            }
            Err(error) => {
                let error = Failure::from(error);
                // A supervisor's renewal outage is retried below, like any other
                // network failure; the UI reports it and waits for the user.
                if !supervised || !matches!(error, Failure::Network) {
                    let state = if matches!(error, Failure::Expired) {
                        Connection::AuthenticationRequired
                    } else {
                        Connection::Failed
                    };
                    publish.set(state, None, Some(error.message()));
                    return;
                }
                (error, None)
            }
        };
        if matches!(error, Failure::Renew) {
            publish.set(Connection::Interrupted, None, None);
            rejected = None;
            continue;
        }
        if matches!(error, Failure::TokenRejected) && rejected.is_none() {
            publish.set(Connection::Interrupted, None, Some(error.message()));
            // Only a connection that held a token can have it rejected.
            rejected = token;
            continue;
        }
        if matches!(
            error,
            Failure::Forbidden | Failure::TokenRejected | Failure::Expired
        ) {
            publish.set(
                if matches!(error, Failure::Expired) {
                    Connection::AuthenticationRequired
                } else {
                    Connection::Forbidden
                },
                None,
                Some(error.message()),
            );
            return;
        }
        if ready_since.is_some_and(|since: Instant| since.elapsed() >= Duration::from_secs(60)) {
            attempts = 0;
        }
        attempts = attempts.saturating_add(1);
        let retry_network = supervised && matches!(error, Failure::Network);
        if (attempts >= 5 && !retry_network)
            || matches!(error, Failure::RateLimited(d) if d > Duration::from_secs(3600))
        {
            publish.set(Connection::Failed, None, Some(error.message()));
            return;
        }
        publish.set(Connection::Interrupted, None, Some(error.message()));
        let backoff = Duration::from_secs((1u64 << (attempts - 1).min(5)).min(30));
        let wait = match error {
            Failure::RateLimited(wait) => wait.max(backoff),
            _ => backoff,
        };
        tokio::time::sleep(wait).await;
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
fn ws_error(error: tungstenite::Error) -> Failure {
    if let tungstenite::Error::Http(response) = error {
        return http_status(response.status(), response.headers())
            .err()
            .unwrap_or(Failure::Protocol);
    }
    match error {
        tungstenite::Error::Protocol(_)
        | tungstenite::Error::Utf8(_)
        | tungstenite::Error::Capacity(_) => Failure::Protocol,
        _ => Failure::Network,
    }
}
async fn send(socket: &mut Socket, command: &str, headers: &[(&str, &str)]) -> Result<(), Failure> {
    let bytes = stomp::encode(command, headers, &[]).map_err(|_| Failure::Protocol)?;
    let text = String::from_utf8(bytes).map_err(|_| Failure::Protocol)?;
    send_ws(socket, Message::text(text)).await
}
/// One bounded WebSocket write; a stalled socket is a network failure.
async fn send_ws(socket: &mut Socket, message: Message) -> Result<(), Failure> {
    timeout(HANDSHAKE_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| Failure::Network)?
        .map_err(ws_error)
}
/// Publishes the current roster as ready, or as forbidden when Epic denied presence.
fn publish_ready(publish: &Publisher, roster: &Roster, presence_forbidden: bool) {
    publish.set(
        if presence_forbidden {
            Connection::Forbidden
        } else {
            Connection::Ready
        },
        Some(roster.friends()),
        presence_forbidden.then_some("backend.epic.friends.presenceForbidden"),
    );
}
async fn get(
    auth: &SocialAuth,
    token: &str,
    base: &str,
    path: &[&str],
    query: &[(&str, &str)],
) -> Result<Value, Failure> {
    let mut url = reqwest::Url::parse(base).map_err(|_| Failure::Protocol)?;
    url.path_segments_mut()
        .map_err(|_| Failure::Protocol)?
        .clear()
        .extend(path.iter().copied());
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    let response = auth
        .api
        .http
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| Failure::Network)?;
    http_status(response.status(), response.headers())?;
    auth::bounded_json(response, 8 * 1024 * 1024)
        .await
        .map_err(Into::into)
}
type ProfileNames = BTreeMap<String, Option<String>>;
struct BootstrapState {
    roster: Roster,
    presence_forbidden: Option<bool>,
    profiles: ProfileNames,
}

async fn register_presence(
    auth: &SocialAuth,
    token: &str,
    account: &str,
    registration: &Registration,
) -> Result<(), Failure> {
    let mut url = reqwest::Url::parse(&auth.api.presence).map_err(|_| Failure::Protocol)?;
    url.path_segments_mut()
        .map_err(|_| Failure::Protocol)?
        .clear()
        .extend([
            "epic",
            "presence",
            "internal",
            "v1",
            "_",
            account,
            "presence",
            &registration.connection,
        ]);
    // Epic requires an automatic connection status. Omitting statusSettings follows
    // the launcher's automatic registration and leaves manual preferences to Epic.
    // The supervisor advertises only the game it owns, on its own connection.
    let mut conn = serde_json::json!({"props": {}});
    if let Some(activity) = &registration.activity {
        let mut running = serde_json::json!({"id": activity.id});
        if let Some(name) = &activity.name {
            running["displayName"] = name.clone().into();
        }
        conn["runningApp"] = running;
    }
    let response = auth
        .api
        .http
        .post(url)
        .bearer_auth(token)
        .header("X-Epic-Agent", "epic-social")
        .header("X-HTTP-Method-Override", "PATCH")
        .json(&serde_json::json!({"status":"online", "conn":conn}))
        .send()
        .await
        .map_err(|_| Failure::Network)?;
    http_status(response.status(), response.headers())?;
    let value: Value = auth::bounded_json(response, 8 * 1024 * 1024).await?;
    if value["own"]["accountId"].as_str() != Some(account) || !value["own"]["status"].is_string() {
        return Err(Failure::Protocol);
    }
    // The reply describes self only. Friends arrive as presence.v1.UPDATE events;
    // successful registration is not evidence that unreported friends are offline.
    Ok(())
}

async fn hydrate_names(
    auth: &SocialAuth,
    token: &str,
    roster: &mut Roster,
    profiles: &mut ProfileNames,
) {
    let friends = roster.friends();
    let accepted: BTreeSet<_> = friends
        .iter()
        .map(|friend| friend.account_id.as_str())
        .collect();
    profiles.retain(|id, _| accepted.contains(id.as_str()));
    let missing: Vec<_> = friends
        .iter()
        .filter(|friend| {
            friend.display_name.is_none() && !profiles.contains_key(&friend.account_id)
        })
        .map(|friend| friend.account_id.as_str())
        .collect();
    // This is optional presentation data, with one bounded request at a time.
    // The socket remains readable while the bootstrap future is suspended here.
    let _ = timeout(Duration::from_secs(10), async {
        for ids in missing.chunks(100) {
            for id in ids {
                profiles.insert((*id).to_owned(), None);
            }
            let query: Vec<_> = ids.iter().map(|id| ("accountId", *id)).collect();
            let Ok(value) = get(
                auth,
                token,
                &auth.api.account,
                &["account", "api", "public", "account"],
                &query,
            )
            .await
            else {
                break;
            };
            let Some(accounts) = value.as_array().filter(|accounts| accounts.len() <= 100) else {
                break;
            };
            for account in accounts {
                let Some(id) = account["id"].as_str().filter(|id| ids.contains(id)) else {
                    continue;
                };
                let name = crate::friends::text(&account["displayName"]).or_else(|| {
                    account["externalAuths"]
                        .as_object()?
                        .values()
                        .find_map(|external| crate::friends::text(&external["externalDisplayName"]))
                });
                profiles.insert(id.to_owned(), name);
            }
        }
    })
    .await;
    roster.apply_profile_names(profiles);
}
async fn bootstrap(
    auth: &SocialAuth,
    token: &str,
    account: &str,
    mut profiles: ProfileNames,
    registration: Option<Registration>,
) -> Result<BootstrapState, Failure> {
    if let Some(registration) = registration.as_ref().filter(|r| r.activity.is_some()) {
        // The detached game supervisor needs only its own presence. Do not gate
        // it on friends permissions or load another copy of the user's roster.
        register_presence(auth, token, account, registration).await?;
        return Ok(BootstrapState {
            roster: Roster::new(account.into()),
            presence_forbidden: Some(false),
            profiles,
        });
    }
    let summary_path = ["friends", "api", "v1", account, "summary"];
    let (summary, presence) = tokio::join!(
        get(
            auth,
            token,
            &auth.api.friends,
            &summary_path,
            &[("displayNames", "true")]
        ),
        async {
            match registration {
                Some(registration) => register_presence(auth, token, account, &registration)
                    .await
                    .map(|()| Some(false)),
                None => Ok(None),
            }
        }
    );
    let mut roster = Roster::new(account.into());
    roster.replace_summary(summary?)?;
    let presence_forbidden = match presence {
        Ok(presence) => presence,
        Err(Failure::Forbidden) => Some(true),
        Err(error) => return Err(error),
    };
    hydrate_names(auth, token, &mut roster, &mut profiles).await;
    Ok(BootstrapState {
        roster,
        presence_forbidden,
        profiles,
    })
}

async fn bounded_bootstrap(
    auth: &SocialAuth,
    token: &str,
    account: &str,
    profiles: ProfileNames,
    registration: Option<Registration>,
) -> Result<BootstrapState, Failure> {
    timeout(
        REQUEST_TIMEOUT,
        bootstrap(auth, token, account, profiles, registration),
    )
    .await
    .map_err(|_| Failure::Network)?
}

type Bootstrap<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<BootstrapState, Failure>> + Send + 'a>,
>;

struct Registration {
    connection: String,
    activity: Option<Activity>,
}

async fn connection(
    auth: &SocialAuth,
    token: &str,
    account: &str,
    publish: &Publisher,
    ready_since: &mut Option<Instant>,
    activity: Option<&Activity>,
) -> Result<(), Failure> {
    let mut request = auth
        .api
        .connect
        .as_str()
        .into_client_request()
        .map_err(|_| Failure::Protocol)?;
    // EGL3 passes the raw access token, without the REST Bearer prefix.
    request.headers_mut().insert(
        "Authorization",
        token.parse().map_err(|_| Failure::Protocol)?,
    );
    // Epic's current gateway omits Sec-WebSocket-Protocol in its HTTP 101 response.
    // Negotiate the protocol in STOMP CONNECT/CONNECTED instead of advertising a
    // WebSocket subprotocol that the gateway does not select.

    let host = request.uri().host().ok_or(Failure::Protocol)?.to_owned();
    let config = WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let (mut socket, response) = timeout(
        HANDSHAKE_TIMEOUT,
        connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Failure::Network)?
    .map_err(ws_error)?;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .is_some_and(|v| v != "v12.stomp")
    {
        return Err(Failure::Protocol);
    }
    send(
        &mut socket,
        "CONNECT",
        &[
            ("accept-version", "1.2"),
            ("host", &host),
            ("heart-beat", "30000,0"),
            // Retain the nonstandard spelling used by the Epic launcher reference.
            ("heartbeat", "30000,0"),
        ],
    )
    .await?;
    let mut decoder = stomp::Decoder::new();
    let mut connected = false;
    let mut subscribed = false;
    let mut roster = Roster::new(account.into());
    let mut ready = false;
    let mut presence_forbidden = false;
    let mut profiles = ProfileNames::new();
    let mut invalidate_profiles = false;
    let mut buffer = Vec::new();
    let mut buffer_bytes = 0usize;
    let mut followup_refresh = false;
    let mut refresh: Option<Bootstrap<'_>> = None;
    let mut outgoing = None;
    let far = || Instant::now() + Duration::from_secs(86400);
    let mut send_deadline = far();
    let setup_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut ping_deadline = Instant::now() + Duration::from_secs(30);
    let mut pong_deadline = None;
    let renewal = Instant::from_std(auth.expires_at().await?);
    loop {
        tokio::select! {
            biased;
            _ = sleep_until(renewal) => return Err(Failure::Renew),
            _ = sleep_until(pong_deadline.unwrap_or_else(far)), if pong_deadline.is_some() => return Err(Failure::Network),
            _ = sleep_until(ping_deadline) => {
                send_ws(&mut socket, Message::Ping(vec![].into())).await?;
                ping_deadline = Instant::now() + Duration::from_secs(30);
                pong_deadline = Some(Instant::now() + Duration::from_secs(20));
            }
            _ = sleep_until(setup_deadline), if !subscribed => return Err(Failure::Protocol),
            _ = sleep_until(send_deadline), if outgoing.is_some() => {
                send_ws(&mut socket, Message::text("\n")).await?;
                send_deadline = Instant::now() + outgoing.unwrap();
            }
            message = socket.next() => {
                let message = message.ok_or(Failure::Network)?.map_err(ws_error)?;
                let bytes = match message {
                    Message::Text(text) => text.as_bytes().to_vec(), Message::Binary(bytes) => bytes.to_vec(),
                    Message::Pong(_) => { pong_deadline = None; continue; }
                    Message::Ping(_) => {
                        timeout(HANDSHAKE_TIMEOUT, socket.flush()).await.map_err(|_| Failure::Network)?.map_err(ws_error)?;
                        continue;
                    }
                    Message::Close(_) => return Err(Failure::Network), _ => continue,
                };
                for frame in decoder.push(&bytes).map_err(|_| Failure::Protocol)? {
                    match frame.command.as_str() {
                        "CONNECTED" if !connected => {
                            if frame.headers.get("version").map(String::as_str) != Some("1.2") { return Err(Failure::Protocol); }
                            connected = true;
                            if let Some(heartbeat) = frame.headers.get("heart-beat") {
                                let (server_out,server_in) = heartbeat.split_once(',').ok_or(Failure::Protocol)?;
                                let server_out: u64 = server_out.parse().map_err(|_| Failure::Protocol)?;
                                let server_in: u64 = server_in.parse().map_err(|_| Failure::Protocol)?;
                                if server_out > 86400000 || server_in > 86400000 { return Err(Failure::Protocol); }
                                outgoing = (server_in > 0).then(|| Duration::from_millis(server_in.max(30000)));
                                // Client advertises zero incoming STOMP heartbeats. The RFC6455
                                // ping/pong watchdog below detects a silent transport instead.
                                if let Some(interval) = outgoing { send_deadline = Instant::now() + interval; }
                                            }
                            let destination = format!("{}/account/{account}", uuid::Uuid::new_v4().simple());
                            send(&mut socket,"SUBSCRIBE", &[("id","sub-0"),("destination",&destination),("ack","auto"),("receipt","friends-ready")]).await?;
                        }
                        "RECEIPT" if connected && frame.headers.get("receipt-id").map(String::as_str) == Some("friends-ready") => {
                            // Wait for the welcome's connection ID before registering presence.
                        }
                        "MESSAGE" if connected => {
                            if frame.headers.get("subscription").map(String::as_str) != Some("sub-0") { continue; }
                            let content = frame.headers.get("content-type").or_else(|| frame.headers.get("Content-Type"));
                            if !content.is_some_and(|v| v.split(';').next() == Some("application/json")) { continue; }
                            // A game connection consumes nothing past the welcome; skip the parse
                            // rather than build and drop a tree for every friend's event.
                            if subscribed && activity.is_some() { continue; }
                            let event: Value = serde_json::from_slice(&frame.body).map_err(|_| Failure::Protocol)?;
                            if event["type"] == "core.connect.v1.connected" {
                                if !event["connectionId"].as_str().is_some_and(|s| !s.is_empty() && s.len() < 4096) { return Err(Failure::Protocol); }
                                // Welcome also proves subscription on servers without receipts.
                                if !subscribed {
                                    subscribed = true;
                                    let registration = Registration {
                                        connection: event["connectionId"].as_str().unwrap().to_owned(),
                                        activity: activity.cloned(),
                                    };
                                    refresh = Some(Box::pin(bounded_bootstrap(auth, token, account, profiles.clone(), Some(registration))));
                                }
                                continue;
                            }
                            if !subscribed { return Err(Failure::Protocol); }
                            let kind = event["type"].as_str().ok_or(Failure::Protocol)?;
                            if !kind.starts_with("friends.v1.") && kind != "presence.v1.UPDATE" { continue; }
                            if matches!(kind, "friends.v1.FRIEND_ENTRY_UPDATED" | "friends.v1.FRIENDSHIP_ENTRY_UPDATE") {
                                profiles.clear();
                                invalidate_profiles = refresh.is_some();
                            }
                            let relations_changed = roster.event(&event)?;
                            if relations_changed {
                                if refresh.is_some() { followup_refresh = true; }
                                else { refresh = Some(Box::pin(bounded_bootstrap(auth, token, account, profiles.clone(), None))); }
                            }
                            if refresh.is_some() {
                                buffer_bytes = buffer_bytes.saturating_add(frame.body.len());
                                if buffer.len() >= MAX_BUFFERED_EVENTS || buffer_bytes > MAX_BUFFERED_BYTES {
                                    // Discard the incomplete attempt and reconnect/resynchronize with backoff.
                                    return Err(Failure::Protocol);
                                }
                                buffer.push(event);
                            }
                            if ready { publish_ready(publish, &roster, presence_forbidden); }
                        }
                        "ERROR" => return Err(Failure::Protocol),
                        _ => return Err(Failure::Protocol),
                    }
                }
            }
            result = async { refresh.as_mut().unwrap().await }, if refresh.is_some() => {
                let loaded = result?;
                let mut new = loaded.roster;
                profiles = loaded.profiles;
                if std::mem::take(&mut invalidate_profiles) { profiles.clear(); }
                if let Some(forbidden) = loaded.presence_forbidden { presence_forbidden = forbidden; }
                // Replay the ordered socket events received since these HTTP reads started.
                // Removed/blocked IDs remain absent, even if the HTTP snapshot was older.
                new.retain_tombstones(&roster);
                new.retain_live_presence(&roster);
                for event in buffer.drain(..) { new.event(&event)?; }
                buffer_bytes = 0;
                refresh = if std::mem::take(&mut followup_refresh) { Some(Box::pin(bounded_bootstrap(auth, token, account, profiles.clone(), None))) } else { None };
                roster = new; ready = true;
                ready_since.get_or_insert_with(Instant::now);
                publish_ready(publish, &roster, presence_forbidden);
            }
        }
    }
}

#[cfg(test)]
mod tests;
