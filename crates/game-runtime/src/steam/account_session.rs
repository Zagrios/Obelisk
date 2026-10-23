//! The Account session of one signed-in Steam account. Steam's client dies
//! with its CM connection, so the session connects again by itself, and
//! everything that holds it reaches Steam through the connection of the
//! moment: the account's installations, chat, its own game, library lookups.
use super::{IdleGuard, Steam, cloud as steam_cloud, content::Network};
use crate::{
    cloud::Control,
    installations::{BoxFuture, Receipt, Transfer},
    protocol::Credentials,
    sessions::Failure,
};
use obelisk_core::{
    Platform, installation::TransferProgress, launch::LaunchTarget, transfer::CancelToken,
    watch::next_matching,
};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    time::Duration,
};
use steam_kit::{
    client::{Client, ClientConnectError, Installation},
    cloud::{CloudListing, CloudMetadata},
    friends::{FriendsConnection, FriendsSnapshot},
};
use tokio::{sync::watch, task::AbortHandle};

/// The waits before each attempt to connect again once the connection
/// dropped: about a minute, so the network a Mac finds again after it wakes,
/// or a Wi-Fi that drops for a while, finds the session still trying.
const WAITS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

/// One CM connection of the session, which ends for good once it drops.
/// Steam's client in production; tests stand in for it.
pub trait Transport: Network {
    /// Its friends after `after_revision`; once it ended, its last ones at once.
    fn next_friends(&self, after_revision: u64) -> BoxFuture<'_, FriendsSnapshot>;
    /// Whether it ended. A request waiting on it, and so the session, learns
    /// of the end only after this tells it.
    fn ended(&self) -> bool;
    /// Ends it now, waking every wait on it and cancelling its download.
    fn shutdown(&self);
}

impl Transport for Client {
    fn next_friends(&self, after_revision: u64) -> BoxFuture<'_, FriendsSnapshot> {
        Box::pin(Client::next_friends(self, after_revision))
    }
    fn ended(&self) -> bool {
        Client::ended(self)
    }
    fn shutdown(&self) {
        Client::shutdown(self);
    }
}

/// Where the session stands with Steam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Its CM connection is up.
    Connected,
    /// Its CM connection dropped, and it connects again.
    Reconnecting,
    /// Every attempt to connect again failed: only `reconnect` tries again.
    Unreachable,
    /// Steam asked the account to sign in again. It is final.
    AuthenticationRequired,
    /// The session was closed. It is final.
    Closed,
}

/// What the session publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// Grows with each change, across CM connections.
    pub revision: u64,
    pub phase: Phase,
    /// The CM connection of the moment: 0 for the first, one more for each
    /// that replaced a dropped one.
    pub transport: u64,
    /// What it, or the one that dropped, last told of the account's friends.
    pub friends: FriendsSnapshot,
}

impl State {
    /// Whether a wait for the state after `after_revision` ends with this
    /// one: a newer one, or at once a closed session's, since UniFFI never
    /// cancels Rust awaits.
    pub fn follows(&self, after_revision: u64) -> bool {
        self.revision != after_revision || self.phase == Phase::Closed
    }

    /// Whether a retry connects again: not once Steam asked the account to
    /// sign in again, nor once the session closed.
    pub fn retries(&self) -> bool {
        !matches!(self.phase, Phase::AuthenticationRequired | Phase::Closed)
    }
}

/// Opens a CM connection of the account.
pub(crate) type Connect<T> =
    Box<dyn Fn() -> BoxFuture<'static, Result<Arc<T>, ClientConnectError>> + Send + Sync>;

/// A CM connection the session follows, and the revision of its friends it
/// already published.
type Following<T> = (Arc<T>, u64);

/// The CM connection of the moment, and the task that follows it.
struct Link<T> {
    transport: Arc<T>,
    /// Grows each time `reconnect` or `close` replaces the cycle under way: a
    /// cycle no longer current changes nothing, and the connection it opens
    /// is shut down.
    cycle: u64,
    /// The current cycle's task, which follows the connection or connects
    /// again.
    task: Option<AbortHandle>,
}

pub struct AccountSession<T: Transport = Client> {
    connect: Connect<T>,
    /// Where cycles run, so that `reconnect` can start one from any thread.
    runtime: tokio::runtime::Handle,
    link: Mutex<Link<T>>,
    /// Sent with the link locked, so a state always names its connection.
    state: watch::Sender<State>,
    /// Lets a cancellation reach a Transfer waiting for the reconnection.
    waiting: Mutex<Option<CancelToken>>,
}

impl AccountSession {
    /// Signs `credentials` in. The first connection's failure is returned as
    /// it is: nothing retries it.
    pub async fn connect(credentials: Credentials) -> Result<Arc<Self>, ClientConnectError> {
        Self::open(Box::new(move || {
            let credentials = credentials.clone();
            Box::pin(async move {
                Client::connect(
                    &credentials.account_name,
                    credentials.steam_id,
                    &credentials.refresh_token,
                )
                .await
                .map(Arc::new)
            })
        }))
        .await
    }
}

impl<T: Transport> AccountSession<T> {
    /// Opens the first connection through `connect`, which every
    /// reconnection uses too.
    pub(crate) async fn open(connect: Connect<T>) -> Result<Arc<Self>, ClientConnectError> {
        let transport = connect().await?;
        let friends = transport.next_friends(0).await;
        let after = friends.revision;
        let (state, _) = watch::channel(State {
            revision: 1,
            phase: Phase::Connected,
            transport: 0,
            friends,
        });
        let session = Arc::new(Self {
            connect,
            runtime: tokio::runtime::Handle::current(),
            link: Mutex::new(Link {
                transport: transport.clone(),
                cycle: 0,
                task: None,
            }),
            state,
            waiting: Mutex::new(None),
        });
        session.spawn(&mut session.link(), Some((transport, after)));
        Ok(session)
    }

    /// The CM connection of the moment. Calls through one that dropped fail
    /// at once, as they always did.
    pub fn current(&self) -> Arc<T> {
        self.link().transport.clone()
    }

    /// The state after `after_revision`. A closed session answers at once,
    /// since UniFFI never cancels Rust awaits.
    pub async fn next(&self, after_revision: u64) -> State {
        let ready = next_matching(self.state.subscribe(), |state| {
            state.follows(after_revision)
        })
        .await;
        match ready {
            Ok(state) | Err(state) => state,
        }
    }

    /// Replaces the CM connection, live or not, and connects again as after a
    /// drop. Nothing once Steam asked the account to sign in again, or once
    /// closed.
    pub fn reconnect(self: &Arc<Self>) {
        let mut link = self.link();
        if !self.state.borrow().retries() {
            return;
        }
        self.end_cycle(&mut link, Phase::Reconnecting);
        self.spawn(&mut link, None);
    }

    /// Stops reconnecting, publishes Closed and shuts the connection down.
    pub fn close(&self) {
        let mut link = self.link();
        if self.state.borrow().phase != Phase::Closed {
            self.end_cycle(&mut link, Phase::Closed);
        }
    }

    fn link(&self) -> MutexGuard<'_, Link<T>> {
        self.link.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn waiting(&self) -> MutexGuard<'_, Option<CancelToken>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes the state `change` makes. The link must be locked.
    fn publish(&self, change: impl FnOnce(&mut State)) {
        self.state.send_modify(|state| {
            change(state);
            state.revision += 1;
        });
    }

    /// Ends the cycle under way, attempt included, publishes `phase`, then
    /// shuts the connection down. Shutting it down stops its download, so
    /// the Transfer whose turn comes next already sees `phase`.
    fn end_cycle(&self, link: &mut Link<T>, phase: Phase) {
        link.cycle += 1;
        if let Some(task) = link.task.take() {
            task.abort();
        }
        self.publish(|state| state.phase = phase);
        link.transport.shutdown();
    }

    /// Starts the link's cycle: it follows `following`, or connects again at
    /// once without it.
    fn spawn(self: &Arc<Self>, link: &mut Link<T>, following: Option<Following<T>>) {
        let task = self
            .runtime
            .spawn(Self::supervise(Arc::downgrade(self), link.cycle, following));
        link.task = Some(task.abort_handle());
    }

    /// Runs `cycle`: follows the connection, connects again once it drops,
    /// and follows the new one. It holds the session only between waits, so
    /// the session can go while it waits.
    async fn supervise(session: Weak<Self>, cycle: u64, mut following: Option<Following<T>>) {
        loop {
            if let Some((transport, after)) = following.take()
                && !Self::follow(&session, cycle, &transport, after).await
            {
                return;
            }
            match Self::connect_again(&session, cycle).await {
                Some(next) => following = Some(next),
                None => return,
            }
        }
    }

    /// Publishes the friends of `transport` after `after` until it ends.
    /// True when it dropped and the session connects again.
    async fn follow(session: &Weak<Self>, cycle: u64, transport: &Arc<T>, mut after: u64) -> bool {
        loop {
            let friends = transport.next_friends(after).await;
            after = friends.revision;
            let Some(session) = session.upgrade() else {
                return false;
            };
            let link = session.link();
            if link.cycle != cycle {
                return false;
            }
            match friends.connection {
                FriendsConnection::Disconnected => {
                    session.publish(|state| {
                        state.phase = Phase::Reconnecting;
                        state.friends = friends;
                    });
                    transport.shutdown();
                    return true;
                }
                FriendsConnection::AuthenticationRequired => {
                    session.publish(|state| {
                        state.phase = Phase::AuthenticationRequired;
                        state.friends = friends;
                    });
                    transport.shutdown();
                    return false;
                }
                _ => session.publish(|state| state.friends = friends),
            }
        }
    }

    /// Connects again, once after each of `WAITS`, and makes the new
    /// connection the one of the moment. None when the cycle ends without
    /// one.
    async fn connect_again(session: &Weak<Self>, cycle: u64) -> Option<Following<T>> {
        for wait in WAITS {
            tokio::time::sleep(wait).await;
            let connecting = (session.upgrade()?.connect)();
            match connecting.await {
                Ok(transport) => {
                    let friends = transport.next_friends(0).await;
                    let after = friends.revision;
                    let Some(session) = session.upgrade() else {
                        transport.shutdown();
                        return None;
                    };
                    let mut link = session.link();
                    if link.cycle != cycle {
                        transport.shutdown();
                        return None;
                    }
                    // The connection it replaces already ended.
                    link.transport = transport.clone();
                    session.publish(|state| {
                        state.phase = Phase::Connected;
                        state.transport += 1;
                        state.friends = friends;
                    });
                    return Some((transport, after));
                }
                Err(ClientConnectError::AuthenticationRequired(_)) => {
                    session
                        .upgrade()?
                        .settle(cycle, Phase::AuthenticationRequired);
                    return None;
                }
                Err(ClientConnectError::Other(_)) => {}
            }
        }
        session.upgrade()?.settle(cycle, Phase::Unreachable);
        None
    }

    /// Publishes `phase` the cycle ended in, unless another replaced it.
    fn settle(&self, cycle: u64, phase: Phase) {
        let link = self.link();
        if link.cycle == cycle {
            self.publish(|state| state.phase = phase);
        }
    }

    /// The connection a Transfer goes through: while the session
    /// reconnects, or once the connection of the moment ended before the
    /// session heard of it, the new one, or the one that dropped once it
    /// gave up. The Transfer keeps its turn in the download queue meanwhile,
    /// so those behind it, of every storefront, wait too. `cancel` stops the
    /// wait.
    async fn serving(&self) -> Result<Arc<T>, Failure> {
        let cancel = CancelToken::new();
        *self.waiting() = Some(cancel.clone());
        let settled = cancel
            .race(next_matching(self.state.subscribe(), |state| {
                match state.phase {
                    Phase::Reconnecting => false,
                    // A Transfer that failed as the connection ended may
                    // hand its turn on before the session publishes the end.
                    Phase::Connected => !self.current().ended(),
                    _ => true,
                }
            }))
            .await;
        self.waiting().take();
        settled
            .map(|_| self.current())
            .map_err(|_| Failure::Cancelled)
    }
}

/// A Transfer whose turn comes while the session reconnects waits for the
/// new connection. Every other call goes through the connection of the
/// moment at once.
impl<T: Transport> Network for AccountSession<T> {
    fn platform<'a>(
        &'a self,
        app_id: u32,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move { self.serving().await?.platform(app_id, language).await })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<Installation, Failure>> {
        Box::pin(async move { self.serving().await?.download(transfer).await })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.current().progress()
    }
    fn cancel(&self) {
        if let Some(waiting) = self.waiting().as_ref() {
            waiting.cancel();
        }
        self.current().cancel();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move { self.current().update_available(receipt).await })
    }
    /// A closed session never connects again.
    fn reconnected(&self, connection: u64) -> BoxFuture<'_, u64> {
        Box::pin(async move {
            let ready = next_matching(self.state.subscribe(), |state| {
                state.phase == Phase::Closed
                    || (state.phase == Phase::Connected && state.transport != connection)
            })
            .await;
            match ready {
                Ok(state) if state.phase == Phase::Connected => state.transport,
                _ => std::future::pending().await,
            }
        })
    }
    fn launch_target<'a>(
        &'a self,
        app_id: u32,
        platform: Platform,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async move {
            self.current()
                .launch_target(app_id, platform, install)
                .await
        })
    }
    fn archive_rules(
        &self,
        app_id: u32,
    ) -> BoxFuture<'_, Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>> {
        Box::pin(async move { self.current().archive_rules(app_id).await })
    }
    fn maintain(
        &self,
        target: steam_cloud::MaintenanceTarget,
        credentials: &Credentials,
        control: Arc<Control>,
        idle: &IdleGuard,
    ) -> Result<(), steam_cloud::Failure> {
        self.current().maintain(target, credentials, control, idle)
    }
}

impl<T: Transport> Drop for AccountSession<T> {
    fn drop(&mut self) {
        let link = self.link.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(task) = link.task.take() {
            task.abort();
        }
        link.transport.shutdown();
    }
}

#[cfg(test)]
mod tests;
