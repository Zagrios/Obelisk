use std::time::{Duration, Instant};

const DELAYS: [u64; 2] = [1, 2];

#[derive(Debug, Eq, PartialEq)]
pub enum Retry {
    Wait(Duration),
    Attempt { remaining: Duration },
    Defer,
}

/// One monotonic budget includes requests and confirmation. Reopening a worker
/// creates a new budget; transient failures and wake do not reset this one.
pub struct RetryBudget {
    deadline: Instant,
    retry_at: Option<Instant>,
    attempts: usize,
    active: bool,
    action_required: bool,
}

impl RetryBudget {
    pub fn new(start: Instant) -> Self {
        Self {
            deadline: start + Duration::from_secs(120),
            retry_at: None,
            attempts: 0,
            active: false,
            action_required: false,
        }
    }

    pub fn poll(&mut self, now: Instant) -> Retry {
        if self.action_required || now >= self.deadline {
            return Retry::Defer;
        }
        if self.active {
            return Retry::Wait(self.deadline - now);
        }
        if self.attempts > 0 {
            let Some(due) = self.retry_at else {
                return Retry::Defer;
            };
            if due >= self.deadline {
                return Retry::Defer;
            }
            if now < due {
                return Retry::Wait(due - now);
            }
        }
        self.active = true;
        self.attempts += 1;
        self.retry_at = None;
        Retry::Attempt {
            remaining: self.deadline - now,
        }
    }

    pub fn failed(&mut self, now: Instant, requires_action: bool) {
        if self.active {
            self.active = false;
            self.retry_at = DELAYS
                .get(self.attempts - 1)
                .map(|delay| now + Duration::from_secs(*delay));
            self.action_required |= requires_action;
        }
    }
}
