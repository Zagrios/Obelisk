//! Bounded reading of a streamed response body, shared by every storefront's
//! requests so the bound, the byte counting and the cancellation stay in one
//! order.
use futures_util::{Stream, StreamExt};

use super::{CancelToken, Cancelled};

/// How long a response body may or must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodySize {
    /// Anything up to this many bytes.
    AtMost(u64),
    /// Exactly this many bytes, such as a chunk whose compressed size the
    /// manifest names.
    Exactly(u64),
}

/// Why a body was not read. Each storefront turns it into its own vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyError {
    /// The connection failed while the body streamed.
    Network,
    /// The advertised or streamed length broke the bound, or an exact body
    /// ended short.
    Size,
    Cancelled,
}

impl From<Cancelled> for BodyError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// Reads a streamed response body within `size`. An advertised length outside
/// it is refused before anything is read; the buffer is reserved only after
/// that check, for the advertised length or the exact size. Each part is
/// passed to `received` before it is checked, so network counters include a
/// refused body's bytes, and a cancellation from inside that callback is seen
/// before the part is kept. Returns `Cancelled` as soon as `cancel` fires,
/// even while a part is pending. `parts` is any byte stream, such as
/// `reqwest::Response::bytes_stream()`; transport errors are opaque.
pub async fn read_body<B: AsRef<[u8]>, X>(
    advertised: Option<u64>,
    parts: impl Stream<Item = Result<B, X>>,
    size: BodySize,
    cancel: &CancelToken,
    mut received: impl FnMut(u64),
) -> Result<Vec<u8>, BodyError> {
    let (bound, reserved) = match size {
        BodySize::AtMost(limit) if advertised.is_some_and(|length| length > limit) => {
            return Err(BodyError::Size);
        }
        BodySize::AtMost(limit) => (limit, advertised.unwrap_or(0)),
        BodySize::Exactly(length) if advertised.is_some_and(|other| other != length) => {
            return Err(BodyError::Size);
        }
        BodySize::Exactly(length) => (length, length),
    };
    let mut body = Vec::with_capacity(usize::try_from(reserved).map_err(|_| BodyError::Size)?);
    let read = async {
        tokio::pin!(parts);
        while let Some(part) = parts.next().await {
            let part = part.map_err(|_| BodyError::Network)?;
            let part = part.as_ref();
            let length = part.len() as u64;
            received(length);
            cancel.check()?;
            // The body never exceeds the bound, so the subtraction cannot wrap.
            if length > bound - body.len() as u64 {
                return Err(BodyError::Size);
            }
            body.extend_from_slice(part);
        }
        Ok(())
    };
    cancel.race(read).await??;
    if let BodySize::Exactly(length) = size
        && body.len() as u64 != length
    {
        return Err(BodyError::Size);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use futures_util::stream;

    use super::*;

    type Part = Result<&'static [u8], ()>;

    async fn read(
        advertised: Option<u64>,
        parts: Vec<Part>,
        size: BodySize,
    ) -> Result<Vec<u8>, BodyError> {
        read_body(
            advertised,
            stream::iter(parts),
            size,
            &CancelToken::new(),
            |_| {},
        )
        .await
    }

    #[tokio::test]
    async fn at_most_refuses_an_advertised_excess_without_reading() {
        let mut counted = 0;
        let body = read_body(
            Some(5),
            stream::iter([Ok::<_, ()>(b"abcde".as_slice())]),
            BodySize::AtMost(4),
            &CancelToken::new(),
            |length| counted += length,
        )
        .await;
        assert_eq!(body, Err(BodyError::Size));
        assert_eq!(counted, 0, "nothing is read past a refused length");
        assert_eq!(
            read(Some(4), vec![Ok(b"ab"), Ok(b"cd")], BodySize::AtMost(4)).await,
            Ok(b"abcd".to_vec())
        );
    }

    #[tokio::test]
    async fn at_most_refuses_a_streamed_excess() {
        assert_eq!(
            read(None, vec![Ok(b"abc"), Ok(b"de")], BodySize::AtMost(4)).await,
            Err(BodyError::Size)
        );
        assert_eq!(
            read(Some(2), vec![Ok(b"abc")], BodySize::AtMost(4)).await,
            Ok(b"abc".to_vec()),
            "an advertised length is a hint inside the bound"
        );
    }

    #[tokio::test]
    async fn exactly_refuses_a_wrong_advertised_length() {
        assert_eq!(
            read(Some(3), vec![Ok(b"abcd")], BodySize::Exactly(4)).await,
            Err(BodyError::Size)
        );
        assert_eq!(
            read(Some(4), vec![Ok(b"abcd")], BodySize::Exactly(4)).await,
            Ok(b"abcd".to_vec())
        );
    }

    #[tokio::test]
    async fn exactly_refuses_a_long_and_a_short_body() {
        assert_eq!(
            read(None, vec![Ok(b"abc"), Ok(b"de")], BodySize::Exactly(4)).await,
            Err(BodyError::Size)
        );
        assert_eq!(
            read(None, vec![Ok(b"abc")], BodySize::Exactly(4)).await,
            Err(BodyError::Size)
        );
        assert_eq!(
            read(None, vec![Ok(b"ab"), Ok(b"cd")], BodySize::Exactly(4)).await,
            Ok(b"abcd".to_vec())
        );
    }

    #[tokio::test]
    async fn every_part_is_counted_before_the_bound_is_checked() {
        let mut counted = Vec::new();
        let body = read_body(
            None,
            stream::iter([Ok::<_, ()>(b"abc".as_slice()), Ok(b"def"), Ok(b"g")]),
            BodySize::AtMost(4),
            &CancelToken::new(),
            |length| counted.push(length),
        )
        .await;
        assert_eq!(body, Err(BodyError::Size));
        assert_eq!(
            counted,
            [3, 3],
            "the refused part reached the network counters"
        );
    }

    #[tokio::test]
    async fn a_cancellation_from_the_received_callback_stops_before_the_part_is_kept() {
        let cancel = CancelToken::new();
        let mut counted = Vec::new();
        let body = read_body(
            None,
            stream::iter([Ok::<_, ()>(b"abc".as_slice()), Ok(b"def")]),
            BodySize::AtMost(16),
            &cancel,
            |length| {
                counted.push(length);
                cancel.cancel();
            },
        )
        .await;
        assert_eq!(body, Err(BodyError::Cancelled));
        assert_eq!(counted, [3], "no part is read after the cancellation");
    }

    #[tokio::test]
    async fn cancellation_returns_while_a_part_is_pending() {
        let cancel = CancelToken::new();
        let reading = read_body(
            Some(4),
            stream::pending::<Part>(),
            BodySize::AtMost(4),
            &cancel,
            |_| {},
        );
        let cancelling = async {
            tokio::task::yield_now().await;
            cancel.cancel();
        };
        let (body, ()) = tokio::join!(reading, cancelling);
        assert_eq!(body, Err(BodyError::Cancelled));
    }

    #[tokio::test]
    async fn a_transport_error_is_network() {
        assert_eq!(
            read(None, vec![Ok(b"ab"), Err(())], BodySize::AtMost(4)).await,
            Err(BodyError::Network)
        );
    }
}
