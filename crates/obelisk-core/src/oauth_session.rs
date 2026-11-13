//! The session of an account whose storefront signs it in with OAuth, from
//! sign-in to sign-out: its credentials and its access token, renewed once it
//! lapsed or the storefront rejected it. Renewals are serialized, while the
//! account's requests release the session, so sign-out discards it promptly.
use std::{future::Future, sync::Arc, time::Instant};

use tokio::sync::Mutex;

use crate::watch::Revisions;

/// What a storefront's OAuth answers a sign-in or a renewal.
// Deliberately no Debug: these are the account's secrets.
pub struct Tokens<C> {
    /// What the next renewal spends, with the account they name. A renewal
    /// may rotate them, and the host keeps the last ones.
    pub credentials: C,
    pub access_token: String,
    /// When the access token lapses.
    pub expires_at: Instant,
}

/// A storefront's OAuth, as an account's session renews through it.
pub trait OAuth: Send + Sync + 'static {
    type Credentials: Clone + Send + Sync + 'static;
    type Error: SessionError + Send + 'static;

    /// Spends `credentials` on a new access token for the same account.
    fn refresh(
        &self,
        credentials: &Self::Credentials,
    ) -> impl Future<Output = Result<Tokens<Self::Credentials>, Self::Error>> + Send;
}

/// How a storefront's errors say what a session answers by itself.
pub trait SessionError {
    /// The account has no session any more.
    fn unauthorized() -> Self;
    /// A renewal ended before it answered.
    fn cancelled() -> Self;
    /// Whether the storefront rejected the access token.
    fn is_unauthorized(&self) -> bool;
}

/// One account's session. Clones share it, such as the work that outlives
/// the call that started it.
pub struct OAuthSession<O: OAuth> {
    shared: Arc<Shared<O>>,
}

struct Shared<O: OAuth> {
    oauth: O,
    /// None once the account signed out.
    tokens: Mutex<Option<Tokens<O::Credentials>>>,
    credential_events: Revisions,
}

impl<O: OAuth> Clone for OAuthSession<O> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<O: OAuth> OAuthSession<O> {
    /// The session `tokens` opened, which renews through `oauth`.
    pub fn new(oauth: O, tokens: Tokens<O::Credentials>) -> Self {
        Self {
            shared: Arc::new(Shared {
                oauth,
                tokens: Mutex::new(Some(tokens)),
                credential_events: Revisions::new(),
            }),
        }
    }

    /// A current access token, renewed when it lapsed or when it is the one
    /// the storefront `rejected`. A rejected token another caller already
    /// renewed is not renewed again.
    pub async fn access_token(&self, rejected: Option<&str>) -> Result<String, O::Error> {
        // The renewing task owns the renewal, not the caller awaiting it: a
        // caller dropped meanwhile never loses credentials the storefront
        // already rotated, and the callers waiting on the session reuse them.
        let shared = Arc::clone(&self.shared);
        let rejected = rejected.map(str::to_owned);
        tokio::spawn(async move {
            let mut session = shared.tokens.lock().await;
            let tokens = session.as_mut().ok_or_else(O::Error::unauthorized)?;
            if tokens.expires_at <= Instant::now()
                || rejected.as_deref() == Some(&tokens.access_token)
            {
                *tokens = shared.oauth.refresh(&tokens.credentials).await?;
                shared.credential_events.advance();
            }
            Ok(tokens.access_token.clone())
        })
        .await
        .map_err(|_| O::Error::cancelled())?
    }

    /// Runs `operation` with a current access token. When the storefront
    /// rejects that token, it is renewed once and the operation runs again.
    /// A result computed for a session the account discarded meanwhile is
    /// never returned: sign-out discards the session concurrently.
    pub async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, O::Error>
    where
        Fut: Future<Output = Result<T, O::Error>>,
    {
        let result = self.with_access_token_detached(operation).await;
        self.check().await?;
        result
    }

    /// As [`Self::with_access_token`], for work that outlives the call that
    /// started it, such as a Transfer's: its result is returned even when
    /// the account discarded its session meanwhile, as the work answers for
    /// that itself, with [`Self::check`], once it is done.
    pub async fn with_access_token_detached<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, O::Error>
    where
        Fut: Future<Output = Result<T, O::Error>>,
    {
        let token = self.access_token(None).await?;
        match operation(token.clone()).await {
            Err(error) if error.is_unauthorized() => {
                operation(self.access_token(Some(&token)).await?).await
            }
            result => result,
        }
    }

    /// Fails once the account discarded its session.
    pub async fn check(&self) -> Result<(), O::Error> {
        match *self.shared.tokens.lock().await {
            Some(_) => Ok(()),
            None => Err(O::Error::unauthorized()),
        }
    }

    /// The credentials the host keeps: those of the last renewal.
    pub async fn credentials(&self) -> Result<O::Credentials, O::Error> {
        let session = self.shared.tokens.lock().await;
        let tokens = session.as_ref().ok_or_else(O::Error::unauthorized)?;
        Ok(tokens.credentials.clone())
    }

    /// When the access token lapses.
    pub async fn expires_at(&self) -> Result<Instant, O::Error> {
        let session = self.shared.tokens.lock().await;
        let tokens = session.as_ref().ok_or_else(O::Error::unauthorized)?;
        Ok(tokens.expires_at)
    }

    /// Signs the account out of this session: every later call fails, and
    /// the tokens it held come back, for a storefront that revokes them. A
    /// renewal in flight ends first, so they are the renewed ones.
    pub async fn discard(&self) -> Option<Tokens<O::Credentials>> {
        self.shared.tokens.lock().await.take()
    }

    /// The revision of the credentials now, which grows with each renewal.
    pub fn credentials_revision(&self) -> u64 {
        self.shared.credential_events.current()
    }

    /// The first revision other than `after`, as soon as a renewal answered,
    /// whatever becomes of the request that needed it; `None` once the watch
    /// stopped.
    pub async fn next_credentials_revision(&self, after: u64) -> Option<u64> {
        self.shared.credential_events.next(after).await
    }

    /// Ends every wait for a next revision, now and later. The session
    /// itself stays.
    pub fn stop_credentials_watch(&self) {
        self.shared.credential_events.stop();
    }
}

#[cfg(test)]
mod tests;
