//! The reconcile-contract compliance suite: an adapter proves it honours
//! [`VenueReconcile`](videre_sdk::VenueReconcile) by implementing
//! [`ReconcileFixture`] over its own mock transport and invoking
//! [`venue_reconcile_compliance!`](crate::venue_reconcile_compliance).

use videre_sdk::{IntentStatus, SubmitOutcome, VenueFault};

use crate::MockFetch;

/// The per-adapter fixtures the compliance suite drives: `program_*` hooks
/// arm a [`MockFetch`], `submit`/`status` run the adapter's paths lifting
/// its error into [`VenueFault`].
pub trait ReconcileFixture {
    /// A signed body the venue accepts and derives a receipt for.
    fn signed_body() -> Vec<u8>;
    /// A pre-sign body the venue accepts.
    fn presign_body() -> Vec<u8>;
    /// The body-derived receipt a held body resolves to.
    fn receipt() -> Vec<u8>;
    /// Arm the mock to accept a fresh submission.
    fn program_accept(fetch: &MockFetch);
    /// Arm the mock to reject a submission as already held.
    fn program_already_held(fetch: &MockFetch);
    /// Arm the mock to answer a status read as not-found.
    fn program_absent(fetch: &MockFetch);
    /// Submit a body through the adapter over `fetch`.
    fn submit(fetch: &MockFetch, body: &[u8]) -> Result<SubmitOutcome, VenueFault>;
    /// Read a receipt's status through the adapter over `fetch`.
    fn status(fetch: &MockFetch, receipt: &[u8]) -> Result<IntentStatus, VenueFault>;
}

/// A fresh submit and a re-POST of a held body resolve identically (re-POST idempotency).
pub fn assert_re_post_idempotent<F: ReconcileFixture>(body: &[u8]) {
    let fresh = MockFetch::default();
    F::program_accept(&fresh);
    let first = F::submit(&fresh, body).expect("a fresh submit is accepted");

    let held = MockFetch::default();
    F::program_already_held(&held);
    let again = F::submit(&held, body).expect("a held body folds to accepted, never a fault");

    assert!(
        first == again,
        "a re-POST of a held body must resolve to the same outcome",
    );
}

/// A held body never surfaces as a terminal fault, across both auth paths.
pub fn assert_held_never_faults<F: ReconcileFixture>() {
    for body in [F::signed_body(), F::presign_body()] {
        let held = MockFetch::default();
        F::program_already_held(&held);
        assert!(
            F::submit(&held, &body).is_ok(),
            "a held body must fold to accepted, never a terminal fault",
        );
    }
}

/// An absent status read stays retryable (`unavailable`), never terminal.
pub fn assert_status_absent_is_retryable<F: ReconcileFixture>() {
    let fetch = MockFetch::default();
    F::program_absent(&fetch);
    assert!(
        matches!(
            F::status(&fetch, &F::receipt()),
            Err(VenueFault::Unavailable(_)),
        ),
        "an absent status read must stay retryable",
    );
}

/// Instantiate the [`VenueReconcile`](videre_sdk::VenueReconcile)
/// compliance suite for a [`ReconcileFixture`]: re-POST idempotency on
/// both auth paths, a held body never faulting, and an absent status read
/// staying retryable.
#[macro_export]
macro_rules! venue_reconcile_compliance {
    ($fixture:ty) => {
        #[test]
        fn reconcile_signed_re_post_is_idempotent() {
            $crate::reconcile::assert_re_post_idempotent::<$fixture>(
                &<$fixture as $crate::reconcile::ReconcileFixture>::signed_body(),
            );
        }

        #[test]
        fn reconcile_presign_re_post_is_idempotent() {
            $crate::reconcile::assert_re_post_idempotent::<$fixture>(
                &<$fixture as $crate::reconcile::ReconcileFixture>::presign_body(),
            );
        }

        #[test]
        fn reconcile_held_body_never_faults() {
            $crate::reconcile::assert_held_never_faults::<$fixture>();
        }

        #[test]
        fn reconcile_status_absent_is_retryable() {
            $crate::reconcile::assert_status_absent_is_retryable::<$fixture>();
        }
    };
}
