use std::{
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Notify, watch};

use super::*;
use crate::watch::next_matching;

#[derive(Clone, Copy, Debug, PartialEq)]
enum FakeError {
    Unauthorized,
    Cancelled,
    /// An outage, or anything else a renewal or a request may end as.
    Network,
}

impl SessionError for FakeError {
    fn unauthorized() -> Self {
        Self::Unauthorized
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn is_unauthorized(&self) -> bool {
        *self == Self::Unauthorized
    }
}

/// How a storefront answers a renewal.
#[derive(Clone, Copy)]
enum Renewal {
    /// The next tokens: `access-1` and `refresh-1` after `access-0` and
    /// `refresh-0`, and so on.
    Rotates,
    Fails(FakeError),
    /// The renewing task dies before it answers.
    Dies,
}

/// A storefront's OAuth that answers as scripted, and logs the refresh
/// tokens it was asked to spend.
#[derive(Clone)]
struct FakeOAuth(Arc<Script>);

struct Script {
    renewal: StdMutex<Renewal>,
    /// Holds each renewal until notified.
    gate: Option<Notify>,
    spent: StdMutex<Vec<String>>,
    /// How many renewals arrived.
    arrived: watch::Sender<usize>,
}

impl FakeOAuth {
    fn new(gate: Option<Notify>) -> Self {
        Self(Arc::new(Script {
            renewal: StdMutex::new(Renewal::Rotates),
            gate,
            spent: StdMutex::new(Vec::new()),
            arrived: watch::channel(0).0,
        }))
    }

    fn answers(&self, renewal: Renewal) {
        *self.0.renewal.lock().unwrap() = renewal;
    }

    /// The refresh tokens spent so far, in order.
    fn spent(&self) -> Vec<String> {
        self.0.spent.lock().unwrap().clone()
    }

    /// Returns once `count` renewals reached the storefront.
    async fn arrived(&self, count: usize) {
        next_matching(self.0.arrived.subscribe(), |arrived| *arrived >= count)
            .await
            .unwrap();
    }

    /// Lets one held renewal answer.
    fn release(&self) {
        self.0.gate.as_ref().unwrap().notify_one();
    }
}

impl OAuth for FakeOAuth {
    type Credentials = String;
    type Error = FakeError;

    async fn refresh(&self, credentials: &String) -> Result<Tokens<String>, FakeError> {
        let renewal = {
            let mut spent = self.0.spent.lock().unwrap();
            spent.push(credentials.clone());
            spent.len()
        };
        self.0.arrived.send_modify(|arrived| *arrived += 1);
        if let Some(gate) = &self.0.gate {
            gate.notified().await;
        }
        let answer = *self.0.renewal.lock().unwrap();
        match answer {
            Renewal::Rotates => Ok(tokens(renewal, Lapses::Later)),
            Renewal::Fails(error) => Err(error),
            Renewal::Dies => panic!("the renewing task dies"),
        }
    }
}

enum Lapses {
    Already,
    Later,
}

/// The tokens of the `renewal`th renewal; those of the sign-in are the 0th.
fn tokens(renewal: usize, lapses: Lapses) -> Tokens<String> {
    Tokens {
        credentials: format!("refresh-{renewal}"),
        access_token: format!("access-{renewal}"),
        expires_at: match lapses {
            Lapses::Already => Instant::now(),
            Lapses::Later => Instant::now() + Duration::from_secs(3600),
        },
    }
}

/// A session just signed in, with the storefront it renews through.
fn signed_in(lapses: Lapses) -> (OAuthSession<FakeOAuth>, FakeOAuth) {
    let oauth = FakeOAuth::new(None);
    (OAuthSession::new(oauth.clone(), tokens(0, lapses)), oauth)
}

/// The same, whose renewals wait for [`FakeOAuth::release`].
fn signed_in_holding_renewals(lapses: Lapses) -> (OAuthSession<FakeOAuth>, FakeOAuth) {
    let oauth = FakeOAuth::new(Some(Notify::new()));
    (OAuthSession::new(oauth.clone(), tokens(0, lapses)), oauth)
}

/// Fails a wait that never ends, in place of the whole run.
async fn bounded<T>(wait: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .expect("the wait ends")
}

/// An operation the storefront rejects `rejections` times, then answers; it
/// logs the access tokens it ran with.
fn rejected_times(
    rejections: usize,
    ran_with: &StdMutex<Vec<String>>,
) -> impl Fn(String) -> std::future::Ready<Result<&'static str, FakeError>> {
    let runs = AtomicUsize::new(0);
    move |token| {
        ran_with.lock().unwrap().push(token);
        std::future::ready(if runs.fetch_add(1, Ordering::SeqCst) < rejections {
            Err(FakeError::Unauthorized)
        } else {
            Ok("answer")
        })
    }
}

#[tokio::test]
async fn a_current_token_is_handed_out_without_a_renewal() {
    let (session, oauth) = signed_in(Lapses::Later);
    assert_eq!(session.access_token(None).await, Ok("access-0".into()));
    assert_eq!(session.credentials().await, Ok("refresh-0".into()));
    assert!(session.expires_at().await.unwrap() > Instant::now());
    assert_eq!(session.check().await, Ok(()));
    assert_eq!(session.credentials_revision(), 1);
    assert!(oauth.spent().is_empty());
}

#[tokio::test]
async fn a_lapsed_token_is_renewed_and_the_credential_revision_advances() {
    let (session, oauth) = signed_in(Lapses::Already);
    assert_eq!(session.access_token(None).await, Ok("access-1".into()));
    assert_eq!(session.credentials().await, Ok("refresh-1".into()));
    assert_eq!(session.credentials_revision(), 2);
    assert_eq!(session.next_credentials_revision(1).await, Some(2));
    // The renewed token is current: nothing renews it again.
    assert_eq!(session.access_token(None).await, Ok("access-1".into()));
    assert_eq!(oauth.spent(), ["refresh-0"]);
}

#[tokio::test]
async fn a_rejected_token_is_renewed_while_it_is_the_current_one_only() {
    let (session, oauth) = signed_in(Lapses::Later);
    let rejected = Some("access-0");
    assert_eq!(session.access_token(rejected).await, Ok("access-1".into()));
    // Another caller's rejection of the same token finds it already renewed.
    assert_eq!(session.access_token(rejected).await, Ok("access-1".into()));
    assert_eq!(oauth.spent(), ["refresh-0"]);
    assert_eq!(session.credentials_revision(), 2);
}

#[tokio::test]
async fn concurrent_callers_share_one_renewal() {
    let (session, oauth) = signed_in_holding_renewals(Lapses::Already);
    let callers = async {
        tokio::join!(
            session.access_token(None),
            session.access_token(None),
            session.access_token(Some("access-0")),
        )
    };
    let renewal = async {
        oauth.arrived(1).await;
        oauth.release();
    };
    let ((first, second, third), ()) = bounded(async { tokio::join!(callers, renewal) }).await;
    assert_eq!(first, Ok("access-1".into()));
    assert_eq!(second, Ok("access-1".into()));
    assert_eq!(third, Ok("access-1".into()));
    assert_eq!(oauth.spent(), ["refresh-0"]);
    assert_eq!(session.credentials_revision(), 2);
}

#[tokio::test]
async fn a_caller_dropped_mid_renewal_loses_nothing_the_storefront_rotated() {
    let (session, oauth) = signed_in_holding_renewals(Lapses::Already);
    {
        let caller = session.access_token(None);
        tokio::pin!(caller);
        tokio::select! {
            biased;
            _ = &mut caller => panic!("the renewal is held"),
            () = bounded(oauth.arrived(1)) => {}
        }
    }
    let rotation = session.next_credentials_revision(1);
    tokio::pin!(rotation);
    tokio::select! {
        biased;
        _ = &mut rotation => panic!("the renewal is held"),
        () = tokio::task::yield_now() => {}
    }
    oauth.release();
    assert_eq!(bounded(rotation).await, Some(2));
    assert_eq!(session.credentials().await, Ok("refresh-1".into()));
    assert_eq!(session.access_token(None).await, Ok("access-1".into()));
    assert_eq!(oauth.spent(), ["refresh-0"]);
}

#[tokio::test]
async fn an_operation_the_storefront_rejects_runs_again_once_with_a_renewed_token() {
    let (session, oauth) = signed_in(Lapses::Later);
    let ran_with = StdMutex::new(Vec::new());
    let answer = session
        .with_access_token(rejected_times(1, &ran_with))
        .await;
    assert_eq!(answer, Ok("answer"));
    assert_eq!(*ran_with.lock().unwrap(), ["access-0", "access-1"]);
    assert_eq!(oauth.spent(), ["refresh-0"]);
    assert_eq!(session.credentials_revision(), 2);
}

#[tokio::test]
async fn a_second_rejection_is_the_answer_and_keeps_the_rotated_credentials() {
    let (session, oauth) = signed_in(Lapses::Later);
    let ran_with = StdMutex::new(Vec::new());
    let answer = session
        .with_access_token(rejected_times(2, &ran_with))
        .await;
    assert_eq!(answer, Err(FakeError::Unauthorized));
    assert_eq!(*ran_with.lock().unwrap(), ["access-0", "access-1"]);
    assert_eq!(oauth.spent(), ["refresh-0"]);
    assert_eq!(session.credentials().await, Ok("refresh-1".into()));
}

#[tokio::test]
async fn any_other_failure_of_an_operation_is_its_answer_at_once() {
    let (session, oauth) = signed_in(Lapses::Later);
    let runs = AtomicUsize::new(0);
    let answer: Result<(), _> = session
        .with_access_token(|_| {
            runs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(FakeError::Network))
        })
        .await;
    assert_eq!(answer, Err(FakeError::Network));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert!(oauth.spent().is_empty());
}

#[tokio::test]
async fn a_renewal_s_credentials_stay_when_the_operation_that_needed_it_fails() {
    let (session, oauth) = signed_in(Lapses::Already);
    let ran_with = StdMutex::new(Vec::new());
    let answer: Result<(), _> = session
        .with_access_token(|token| {
            ran_with.lock().unwrap().push(token);
            std::future::ready(Err(FakeError::Network))
        })
        .await;
    assert_eq!(answer, Err(FakeError::Network));
    assert_eq!(*ran_with.lock().unwrap(), ["access-1"]);
    assert_eq!(session.credentials().await, Ok("refresh-1".into()));
    assert_eq!(session.credentials_revision(), 2);
    assert_eq!(oauth.spent(), ["refresh-0"]);
}

#[tokio::test]
async fn a_result_for_a_session_discarded_meanwhile_is_never_returned() {
    for answer in [Ok("answer"), Err(FakeError::Network)] {
        let (session, _) = signed_in(Lapses::Later);
        let session = &session;
        let signing_out = |_: String| async move {
            session.discard().await;
            answer
        };
        assert_eq!(
            session.with_access_token(signing_out).await,
            Err(FakeError::Unauthorized)
        );
    }
    // Work that outlives its call answers for the sign-out itself.
    let (session, _) = signed_in(Lapses::Later);
    let session = &session;
    let signing_out = |_: String| async move {
        session.discard().await;
        Ok("answer")
    };
    assert_eq!(
        session.with_access_token_detached(signing_out).await,
        Ok("answer")
    );
    assert_eq!(session.check().await, Err(FakeError::Unauthorized));
}

#[tokio::test]
async fn a_discarded_session_answers_nothing_and_renews_nothing() {
    let (session, oauth) = signed_in(Lapses::Already);
    let held = session.discard().await.expect("the tokens of the sign-in");
    assert_eq!(held.access_token, "access-0");
    assert_eq!(held.credentials, "refresh-0");
    assert!(session.discard().await.is_none());

    let unauthorized = Err(FakeError::Unauthorized);
    assert_eq!(session.access_token(None).await, unauthorized);
    assert_eq!(session.credentials().await, unauthorized);
    assert_eq!(
        session.expires_at().await.err(),
        Some(FakeError::Unauthorized)
    );
    assert_eq!(session.check().await, Err(FakeError::Unauthorized));
    let ran_with = StdMutex::new(Vec::new());
    for answer in [
        session
            .with_access_token(rejected_times(0, &ran_with))
            .await,
        session
            .with_access_token_detached(rejected_times(0, &ran_with))
            .await,
    ] {
        assert_eq!(answer, Err(FakeError::Unauthorized));
    }
    assert!(ran_with.lock().unwrap().is_empty());
    assert!(oauth.spent().is_empty());
    assert_eq!(session.credentials_revision(), 1);
}

#[tokio::test]
async fn a_sign_out_during_a_renewal_discards_the_renewed_tokens() {
    let (session, oauth) = signed_in_holding_renewals(Lapses::Already);
    let caller = session.access_token(None);
    let sign_out = async {
        oauth.arrived(1).await;
        let discarding = session.discard();
        tokio::pin!(discarding);
        tokio::select! {
            biased;
            _ = &mut discarding => panic!("the renewal holds the session"),
            () = tokio::task::yield_now() => {}
        }
        oauth.release();
        discarding.await
    };
    let (token, held) = bounded(async { tokio::join!(caller, sign_out) }).await;
    assert_eq!(token, Ok("access-1".into()));
    assert_eq!(held.expect("the renewed tokens").access_token, "access-1");
    assert_eq!(session.check().await, Err(FakeError::Unauthorized));
}

#[tokio::test]
async fn a_failed_renewal_is_the_answer_and_leaves_the_session_as_it_was() {
    let (session, oauth) = signed_in(Lapses::Already);
    oauth.answers(Renewal::Fails(FakeError::Network));
    assert_eq!(session.access_token(None).await, Err(FakeError::Network));
    assert_eq!(session.credentials().await, Ok("refresh-0".into()));
    assert_eq!(session.credentials_revision(), 1);
    assert_eq!(session.check().await, Ok(()));
    // No operation runs without a token.
    let ran_with = StdMutex::new(Vec::new());
    let answer = session
        .with_access_token(rejected_times(0, &ran_with))
        .await;
    assert_eq!(answer, Err(FakeError::Network));
    assert!(ran_with.lock().unwrap().is_empty());
    // The next caller asks the storefront again, with the same credentials.
    oauth.answers(Renewal::Rotates);
    assert_eq!(session.access_token(None).await, Ok("access-3".into()));
    assert_eq!(oauth.spent(), ["refresh-0", "refresh-0", "refresh-0"]);
    assert_eq!(session.credentials_revision(), 2);
}

#[tokio::test]
async fn a_rejected_operation_whose_renewal_fails_does_not_run_again() {
    let (session, oauth) = signed_in(Lapses::Later);
    oauth.answers(Renewal::Fails(FakeError::Unauthorized));
    let ran_with = StdMutex::new(Vec::new());
    let answer = session
        .with_access_token(rejected_times(1, &ran_with))
        .await;
    assert_eq!(answer, Err(FakeError::Unauthorized));
    assert_eq!(*ran_with.lock().unwrap(), ["access-0"]);
    assert_eq!(oauth.spent(), ["refresh-0"]);
    assert_eq!(session.credentials().await, Ok("refresh-0".into()));
}

#[tokio::test]
async fn a_renewal_that_dies_answers_cancelled_and_leaves_the_session_as_it_was() {
    let (session, oauth) = signed_in(Lapses::Already);
    oauth.answers(Renewal::Dies);
    assert_eq!(session.access_token(None).await, Err(FakeError::Cancelled));
    assert_eq!(session.credentials().await, Ok("refresh-0".into()));
    assert_eq!(session.credentials_revision(), 1);
    oauth.answers(Renewal::Rotates);
    assert_eq!(session.access_token(None).await, Ok("access-2".into()));
}

#[tokio::test]
async fn stopping_the_credentials_watch_ends_its_waits_and_keeps_the_session() {
    let (session, _) = signed_in(Lapses::Later);
    let waiting = session.next_credentials_revision(1);
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        _ = &mut waiting => panic!("no renewal answered"),
        () = tokio::task::yield_now() => {}
    }
    session.clone().stop_credentials_watch();
    assert_eq!(bounded(waiting).await, None);
    assert_eq!(session.next_credentials_revision(1).await, None);
    assert_eq!(session.access_token(None).await, Ok("access-0".into()));
}
