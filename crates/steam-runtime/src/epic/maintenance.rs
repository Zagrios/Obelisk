//! Cloud maintenance of one Epic game inside Forge. The sync reports to the
//! attempt that observes it, not to a supervisor journal.
use super::*;

/// A question left unanswered this long ends the maintenance attempt.
const QUESTION_DEADLINE: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceStatus {
    pub cloud_phase: CloudPhase,
    pub decision_id: u64,
    pub error: Option<String>,
    pub allowed_choices: Vec<Choice>,
}

struct Question {
    status: MaintenanceStatus,
    answer: Option<Choice>,
}

pub struct MaintenanceChannel {
    cancel: CancelToken,
    state: Mutex<Question>,
    changed: Condvar,
    publish: Box<dyn Fn(MaintenanceStatus) + Send + Sync>,
}

impl MaintenanceChannel {
    pub fn new(publish: impl Fn(MaintenanceStatus) + Send + Sync + 'static) -> Self {
        Self {
            cancel: CancelToken::new(),
            state: Mutex::new(Question {
                status: MaintenanceStatus {
                    cloud_phase: CloudPhase::Checking,
                    decision_id: 0,
                    error: None,
                    allowed_choices: Vec::new(),
                },
                answer: None,
            }),
            changed: Condvar::new(),
            publish: Box::new(publish),
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> MaintenanceStatus {
        self.state.lock().unwrap().status.clone()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
        self.changed.notify_all();
    }

    /// Accepts one answer to the question `decision_id` still shows.
    pub fn answer(&self, decision_id: u64, choice: Choice) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.status.decision_id != decision_id
            || !state.status.allowed_choices.contains(&choice)
            || state.answer.is_some()
        {
            return Err(Error::Session);
        }
        state.answer = Some(choice);
        self.changed.notify_all();
        Ok(())
    }

    fn set(&self, change: impl FnOnce(&mut Question)) {
        let status = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.status.clone()
        };
        (self.publish)(status);
    }
}

impl CloudChannel for MaintenanceChannel {
    fn cancellation(&self) -> &CancelToken {
        &self.cancel
    }

    fn launching(&self) -> bool {
        false
    }

    fn show(&self, phase: CloudPhase, error: Option<String>) -> Result<()> {
        self.set(|state| {
            state.status.cloud_phase = phase;
            state.status.error = error;
            state.status.allowed_choices.clear();
        });
        Ok(())
    }

    fn question(
        &self,
        phase: CloudPhase,
        error: Option<String>,
        choices: Vec<Choice>,
    ) -> Result<Choice> {
        self.set(|state| {
            state.answer = None;
            state.status.decision_id += 1;
            state.status.cloud_phase = phase;
            state.status.error = error;
            state.status.allowed_choices = choices;
        });
        let deadline = Instant::now() + QUESTION_DEADLINE;
        let mut state = self.state.lock().unwrap();
        let choice = loop {
            if self.cancel.is_cancelled() || Instant::now() >= deadline {
                break Err(Error::Cancelled);
            }
            if let Some(choice) = state.answer.take() {
                break Ok(choice);
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_secs(1))
                .unwrap()
                .0;
        };
        drop(state);
        self.set(|state| state.status.allowed_choices.clear());
        choice
    }
}
