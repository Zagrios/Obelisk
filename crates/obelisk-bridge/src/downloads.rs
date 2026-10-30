//! The process's one download queue, as Swift presents and steers it. Every
//! account's installations queue their Transfers here.
use std::sync::{Arc, OnceLock};

use game_runtime::queue::{self, DownloadQueue};

use crate::SessionStorefront;

/// Sets the speed used by the next Steam content download. Zero is unlimited.
#[uniffi::export]
pub fn set_steam_download_limit_mbps(value: u64) {
    steam_kit::content::set_download_limit_mbps(value);
}

/// The queue every account's installations share.
pub(crate) fn queue() -> &'static Arc<DownloadQueue> {
    static QUEUE: OnceLock<Arc<DownloadQueue>> = OnceLock::new();
    QUEUE.get_or_init(DownloadQueue::new)
}

/// What a queued Transfer does to its game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DownloadKind {
    Install,
    Update,
    Repair,
    /// The installation moves into the games folder.
    Move,
}

/// One Transfer in the queue.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DownloadItem {
    pub id: u64,
    pub storefront: SessionStorefront,
    pub account: String,
    /// The game, as its storefront's installations name it.
    pub game: String,
    pub name: String,
    pub kind: DownloadKind,
    /// Obelisk queued it for a newer build, not the player.
    pub automatic: bool,
    /// A launch waits for it: it goes first and cannot be paused.
    pub launching: bool,
    pub paused: bool,
}

/// Everything the queue presents. `revision` grows with every change.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DownloadQueueState {
    pub revision: u64,
    pub running: Option<DownloadItem>,
    /// In the order they will run.
    pub waiting: Vec<DownloadItem>,
    /// A game runs: automatic updates wait.
    pub held: bool,
}

impl From<queue::Item> for DownloadItem {
    fn from(item: queue::Item) -> Self {
        Self {
            id: item.id,
            storefront: match item.storefront {
                queue::Storefront::Steam => SessionStorefront::Steam,
                queue::Storefront::Epic => SessionStorefront::Epic,
                queue::Storefront::Gog => SessionStorefront::Gog,
            },
            account: item.account,
            game: item.game,
            name: item.name,
            kind: match item.kind {
                queue::Kind::Install => DownloadKind::Install,
                queue::Kind::Update => DownloadKind::Update,
                queue::Kind::Repair => DownloadKind::Repair,
                queue::Kind::Move => DownloadKind::Move,
            },
            automatic: item.automatic,
            launching: item.launching,
            paused: item.paused,
        }
    }
}

impl From<queue::View> for DownloadQueueState {
    fn from(view: queue::View) -> Self {
        Self {
            revision: view.revision,
            running: view.running.map(Into::into),
            waiting: view.waiting.into_iter().map(Into::into).collect(),
            held: view.held,
        }
    }
}

/// The process's download queue.
#[derive(uniffi::Object)]
pub struct Downloads {
    queue: Arc<DownloadQueue>,
}

#[uniffi::export(async_runtime = "tokio")]
impl Downloads {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: queue().clone(),
        })
    }

    pub fn state(&self) -> DownloadQueueState {
        self.queue.view().into()
    }

    /// Waits for a revision newer than `after`.
    pub async fn changed(&self, after: u64) -> DownloadQueueState {
        self.queue.changed(after).await.into()
    }

    /// Takes the Transfer out of the queue, stopping it when it runs.
    pub fn remove(&self, id: u64) {
        self.queue.remove(id)
    }

    /// Pauses the Transfer: the running one stops and waits first.
    pub fn pause(&self, id: u64) {
        self.queue.pause(id)
    }

    pub fn resume(&self, id: u64) {
        self.queue.resume(id)
    }

    /// Moves the waiting Transfer first. The one under way goes on.
    pub fn move_to_top(&self, id: u64) {
        self.queue.move_to_top(id)
    }
}
