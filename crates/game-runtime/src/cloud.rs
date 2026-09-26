//! What a Cloud sync publishes and asks, shared by every storefront. A Game
//! session journals its status; Cloud maintenance keeps it in SailOver's memory.
//! Only the publication differs.
pub(crate) mod sync;

use crate::supervisor::session::Questions;
use sailover_core::transfer::CancelToken;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Nothing was published yet.
    #[default]
    Idle,
    Checking,
    Ready,
    Syncing,
    Synced,
    Conflict,
    Pending,
    /// Cloud is turned off in the storefront's preferences.
    Disabled,
    /// The game's Cloud rules are not supported on macOS.
    Unsupported,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Choice {
    Retry,
    Cancel,
    LaunchAnyway,
    Local,
    Cloud,
}

/// A sync's progress and the Cloud question it waits on, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub phase: Phase,
    /// Binds an answer to the question this status shows. Shared with the
    /// Rosetta requirement of a Game session. Steam journals named it `id`.
    #[serde(alias = "id")]
    pub decision_id: u64,
    /// Localization key of the failure the status reports.
    #[serde(default)]
    pub error: Option<String>,
    /// Empty unless a question waits. Presentation never derives choices
    /// from the phase or the error.
    #[serde(default)]
    pub allowed_choices: Vec<Choice>,
}

/// A Cloud question: what the player chooses between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Question {
    /// Local and Cloud saves both changed.
    Conflict,
    /// A launch check failed after its retries.
    Failed { launch_anyway: bool },
}
impl Question {
    fn choices(self) -> Vec<Choice> {
        match self {
            Self::Conflict => vec![Choice::Local, Choice::Cloud, Choice::Cancel],
            Self::Failed { launch_anyway } => {
                let mut choices = vec![Choice::Retry, Choice::Cancel];
                if launch_anyway {
                    choices.push(Choice::LaunchAnyway);
                }
                choices
            }
        }
    }
}

/// Why a sync must stop now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Cancelled, answered Cancel, or abandoned by its observers.
    Cancelled,
    /// Account revocation: network work stops, the game keeps running.
    Revoked,
}

/// The answer no longer fits the question shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the answer does not fit the question shown")]
pub struct Rejected;

struct State {
    status: Status,
    answer: Option<Choice>,
}

/// One sync's status, question, cancellation and revocation.
pub struct Control {
    questions: Arc<Questions>,
    /// Interrupts network work on cancellation and revocation alike.
    cancellation: CancelToken,
    cancelled: AtomicBool,
    revoked: AtomicBool,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    publish: Box<dyn Fn(Status) + Send + Sync>,
}
impl Control {
    /// Cloud maintenance in SailOver, where the UI waits for any answer.
    pub fn new(publish: impl Fn(Status) + Send + Sync + 'static) -> Self {
        Self::with_questions(Arc::new(Questions::in_app()), publish)
    }
    /// A Game session's sync, whose questions share its decision counter and
    /// are abandoned once nobody observes the session.
    pub(crate) fn with_questions(
        questions: Arc<Questions>,
        publish: impl Fn(Status) + Send + Sync + 'static,
    ) -> Self {
        Self {
            questions,
            cancellation: CancelToken::new(),
            cancelled: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            state: Mutex::new(State {
                status: Status::default(),
                answer: None,
            }),
            changed: tokio::sync::Notify::new(),
            publish: Box::new(publish),
        }
    }

    /// Accepts one answer to the question `decision_id` still shows.
    pub fn answer(&self, decision_id: u64, choice: Choice) -> Result<(), Rejected> {
        let mut state = self.state.lock().unwrap();
        if state.status.decision_id != decision_id
            || state.answer.is_some()
            || !state.status.allowed_choices.contains(&choice)
        {
            return Err(Rejected);
        }
        state.answer = Some(choice);
        // The question is closed once answered, before the sync resumes.
        state.status.allowed_choices.clear();
        (self.publish)(state.status.clone());
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancellation.cancel();
        self.changed.notify_waiters();
    }
    /// Stops the account's network work. The caller checks the account.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        self.cancellation.cancel();
        self.changed.notify_waiters();
    }
    pub fn revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
    /// For storefront calls that stop on a token.
    pub fn cancellation(&self) -> &CancelToken {
        &self.cancellation
    }
    pub fn check(&self) -> Result<(), Stop> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Stop::Cancelled)
        } else if self.revoked.load(Ordering::Acquire) {
            Err(Stop::Revoked)
        } else if self.cancellation.is_cancelled() {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Publishes progress. Any question shown is closed.
    pub fn show(&self, phase: Phase, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.status.phase = phase;
        state.status.error = error;
        state.status.allowed_choices.clear();
        state.answer = None;
        // Published under the lock, so observers see statuses in order.
        (self.publish)(state.status.clone());
    }

    /// Asks the player and waits for the answer. Cancel, cancellation and an
    /// abandoned question stop the sync; so does revocation.
    pub async fn ask(&self, question: Question, error: Option<String>) -> Result<Choice, Stop> {
        {
            let mut state = self.state.lock().unwrap();
            state.status = Status {
                phase: match question {
                    Question::Conflict => Phase::Conflict,
                    Question::Failed { .. } => Phase::Failed,
                },
                decision_id: self.questions.next(),
                error,
                allowed_choices: question.choices(),
            };
            state.answer = None;
            (self.publish)(state.status.clone());
        }
        // Abandoned after its deadline, or once nobody observes the session.
        let published = Instant::now();
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.check()?;
            if let Some(choice) = self.state.lock().unwrap().answer.take() {
                return if choice == Choice::Cancel {
                    Err(Stop::Cancelled)
                } else {
                    Ok(choice)
                };
            }
            if self.questions.abandoned(published) {
                return Err(Stop::Cancelled);
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), changed).await;
        }
    }

    /// Paces a retry; cancellation and revocation cut it short.
    pub async fn wait(&self, duration: Duration) -> Result<(), Stop> {
        self.check()?;
        tokio::select! {
            _ = tokio::time::sleep(duration) => (),
            _ = self.cancellation.cancelled() => (),
        }
        self.check()
    }

    /// Runs `future` until it ends, or until cancellation or revocation.
    pub async fn interruptible<F: std::future::Future>(
        &self,
        future: F,
    ) -> Result<F::Output, Stop> {
        self.check()?;
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => {
                self.check()?;
                Err(Stop::Cancelled)
            }
            output = future => Ok(output),
        }
    }
}

#[cfg(test)]
impl Control {
    /// The status last published, as its observer holds it.
    pub(crate) fn status(&self) -> Status {
        self.state.lock().unwrap().status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded() -> (Arc<Mutex<Vec<Status>>>, Control) {
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = published.clone();
        let control = Control::new(move |status| sink.lock().unwrap().push(status));
        (published, control)
    }

    #[tokio::test]
    async fn only_published_choices_are_accepted_once() {
        for question in [
            Question::Conflict,
            Question::Failed {
                launch_anyway: false,
            },
            Question::Failed {
                launch_anyway: true,
            },
        ] {
            let (published, control) = recorded();
            let control = Arc::new(control);
            let asking = tokio::spawn({
                let control = control.clone();
                async move { control.ask(question, Some("fixture.failure".into())).await }
            });
            let status = loop {
                if let Some(status) = published.lock().unwrap().last().cloned() {
                    break status;
                }
                tokio::task::yield_now().await;
            };
            assert_eq!(status.allowed_choices, question.choices());
            assert_eq!(status.error.as_deref(), Some("fixture.failure"));
            for choice in [
                Choice::Retry,
                Choice::Local,
                Choice::Cloud,
                Choice::LaunchAnyway,
                Choice::Cancel,
            ] {
                if !status.allowed_choices.contains(&choice) {
                    assert_eq!(control.answer(status.decision_id, choice), Err(Rejected));
                }
            }
            assert_eq!(
                control.answer(status.decision_id + 1, status.allowed_choices[0]),
                Err(Rejected)
            );
            control
                .answer(status.decision_id, status.allowed_choices[0])
                .unwrap();
            assert_eq!(
                control.answer(status.decision_id, status.allowed_choices[0]),
                Err(Rejected),
                "a question takes one answer"
            );
            let closed = published.lock().unwrap().last().cloned().unwrap();
            assert!(closed.allowed_choices.is_empty());
            assert_eq!(closed.decision_id, status.decision_id);
            assert_eq!(asking.await.unwrap(), Ok(status.allowed_choices[0]));
        }
    }

    #[tokio::test]
    async fn progress_keeps_the_decision_and_closes_the_question() {
        let (published, control) = recorded();
        control.show(Phase::Checking, None);
        let first = published.lock().unwrap().last().cloned().unwrap();
        assert_eq!(first.decision_id, 0);
        assert!(control.answer(0, Choice::Retry).is_err());
        let control = Arc::new(control);
        let asking = tokio::spawn({
            let control = control.clone();
            async move { control.ask(Question::Conflict, None).await }
        });
        let question = loop {
            let last = published.lock().unwrap().last().cloned().unwrap();
            if !last.allowed_choices.is_empty() {
                break last;
            }
            tokio::task::yield_now().await;
        };
        control
            .answer(question.decision_id, Choice::Cancel)
            .unwrap();
        assert_eq!(asking.await.unwrap(), Err(Stop::Cancelled));
        control.show(Phase::Pending, Some("fixture.pending".into()));
        let pending = published.lock().unwrap().last().cloned().unwrap();
        assert_eq!(pending.decision_id, question.decision_id);
        assert!(pending.allowed_choices.is_empty());
        assert!(control.answer(pending.decision_id, Choice::Local).is_err());
    }

    #[tokio::test]
    async fn revocation_wakes_a_pending_question_and_stops_network_work() {
        let (_, control) = recorded();
        let control = Arc::new(control);
        let asking = tokio::spawn({
            let control = control.clone();
            async move { control.ask(Question::Conflict, None).await }
        });
        tokio::task::yield_now().await;
        control.revoke();
        assert_eq!(asking.await.unwrap(), Err(Stop::Revoked));
        assert!(control.revoked());
        assert!(control.cancellation().is_cancelled());
        assert_eq!(control.check(), Err(Stop::Revoked));
        assert_eq!(
            control.interruptible(std::future::pending::<()>()).await,
            Err(Stop::Revoked)
        );
    }

    #[tokio::test]
    async fn cancellation_cuts_a_wait_short_and_wins_over_revocation() {
        let (_, control) = recorded();
        let control = Arc::new(control);
        let waiting = tokio::spawn({
            let control = control.clone();
            async move { control.wait(Duration::from_secs(60)).await }
        });
        tokio::task::yield_now().await;
        control.cancel();
        assert_eq!(waiting.await.unwrap(), Err(Stop::Cancelled));
        control.revoke();
        assert_eq!(control.check(), Err(Stop::Cancelled));
    }

    #[test]
    fn earlier_status_journals_still_read() {
        let steam: Status = serde_json::from_value(serde_json::json!({
            "phase": "Conflict", "id": 4, "error": "backend.steam.cloud.conflict",
            "allowed_choices": ["Local", "Cloud", "Cancel"],
        }))
        .unwrap();
        assert_eq!(steam.decision_id, 4);
        assert_eq!(steam.phase, Phase::Conflict);
        let epic: Status = serde_json::from_value(serde_json::json!({
            "phase": "Unsupported", "decision_id": 2, "allowed_choices": [],
        }))
        .unwrap();
        assert_eq!(epic.decision_id, 2);
        assert_eq!(epic.phase, Phase::Unsupported);
        assert_eq!(epic.error, None);
    }
}
