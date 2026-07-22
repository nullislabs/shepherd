//! Futures on the synchronous guest boundary.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Complete a future in one poll. Guest host imports are synchronous,
/// so every await in a keeper future resolves immediately and one poll
/// runs it to completion. `None` reports a future that suspended,
/// which nothing built over the host imports does; the keeper macro's
/// emitted glue folds it to a typed fault.
pub fn complete<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

#[cfg(test)]
mod tests {
    use super::complete;

    #[test]
    fn ready_chain_completes_in_one_poll() {
        async fn two() -> u8 {
            let one = async { 1u8 }.await;
            one + async { 1u8 }.await
        }
        assert_eq!(complete(two()), Some(2));
    }

    #[test]
    fn suspending_future_reports_none() {
        assert_eq!(complete(std::future::pending::<()>()), None);
    }
}
