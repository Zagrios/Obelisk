//! Steam presence belongs to the native host that owns the game session.
use crate::Runtime;
use std::sync::{Arc, Mutex};

/// Sends one presence edge through the game's host. Tests record it instead.
pub(crate) type Publish = Box<dyn Fn(&Runtime, bool) + Send + Sync>;

enum State {
    Pending,
    Active(Arc<Runtime>),
    Stopped,
}

pub(crate) struct GamePresence {
    state: Mutex<State>,
    publish: Publish,
}

impl Default for GamePresence {
    fn default() -> Self {
        Self::new(Box::new(|runtime, online| {
            let _ = runtime.set_presence(online);
        }))
    }
}

impl GamePresence {
    pub(crate) fn new(publish: Publish) -> Self {
        Self {
            state: Mutex::new(State::Pending),
            publish,
        }
    }
    pub(crate) fn start(&self, runtime: Arc<Runtime>) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, State::Pending) {
            (self.publish)(&runtime, true);
            *state = State::Active(runtime);
        }
    }
    pub(crate) fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        if let State::Active(runtime) = &*state {
            (self.publish)(runtime, false);
        }
        // Revocation before exec permanently prevents a later Online command.
        *state = State::Stopped;
    }
}
impl Drop for GamePresence {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(root: &std::path::Path) -> Arc<Runtime> {
        let output = root
            .join("commands")
            .to_string_lossy()
            .replace('\'', "'\\''");
        Arc::new(crate::tests::runtime(&format!("cat > '{output}'"), root))
    }

    #[test]
    fn presence_uses_the_game_host_and_revocation_does_not_stop_it() {
        let root = tempfile::tempdir().unwrap();
        let runtime = host(root.path());
        let presence = Arc::new(GamePresence::default());
        presence.start(runtime.clone());
        presence.start(runtime.clone());
        let revoking = presence.clone();
        let revoke = std::thread::spawn(move || revoking.stop());
        presence.stop();
        revoke.join().unwrap();
        presence.start(runtime.clone());
        assert!(runtime.inner.lock().unwrap().host.alive().unwrap());
        runtime.stopped(None);
        assert_eq!(std::fs::read(root.path().join("commands")).unwrap(), [1, 0]);
    }

    #[test]
    fn revocation_before_exec_prevents_later_native_publication() {
        let root = tempfile::tempdir().unwrap();
        let runtime = host(root.path());
        let presence = GamePresence::default();
        presence.stop();
        presence.start(runtime.clone());
        runtime.stopped(None);
        assert!(
            std::fs::read(root.path().join("commands"))
                .unwrap()
                .is_empty()
        );
    }
}
