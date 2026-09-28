//! Regression tests for the #1778 ack-reaction cooldown gate.
//!
//! During a global 429 cooldown, cosmetic ack reactions ("👀" seen-marker)
//! must be suppressed: the API rejects them with `Retry after` and every
//! rejected call risks extending the flood ban (observed occurrences: 4
//! "failed to set intermediate reaction: Retry after 5s" WARNs inside an
//! active cooldown, 2026-09-28 logs). The gate decision lives in
//! `rate_limit::reaction_ack_permitted`, consulted by `fire_reaction`.
//!
//! These live here rather than inline in `channels/telegram/` so that all
//! tests stay under `src/tests/` per the test-isolation rule.
use std::time::Duration;

use crate::channels::telegram::governor::test_support;
use crate::channels::telegram::rate_limit::{
    reaction_ack_permitted, record_global_429, reset_global_cooldown,
};

#[tokio::test]
async fn ack_suppressed_while_global_cooldown_active() {
    let _guard = test_support::registry_guard().await;
    test_support::reset(0);
    reset_global_cooldown();

    assert!(reaction_ack_permitted(), "clear state must permit acks");

    // Activate the same flood state the send path records on a 429.
    record_global_429(Duration::from_secs(5));
    assert!(
        !reaction_ack_permitted(),
        "ack reactions must be suppressed while the global 429 cooldown is active"
    );

    reset_global_cooldown();
}

#[tokio::test]
async fn ack_permitted_again_after_cooldown_expires() {
    let _guard = test_support::registry_guard().await;
    test_support::reset(0);
    reset_global_cooldown();

    // 1s requested + 2s margin = 3s deadline on the virtual clock.
    record_global_429(Duration::from_secs(1));
    assert!(!reaction_ack_permitted());

    // Advance the virtual clock past the deadline: reactions resume without
    // any explicit reset, proving the gate is deadline-driven, not sticky.
    test_support::advance(3000);
    assert!(
        reaction_ack_permitted(),
        "acks must resume once the cooldown deadline passes"
    );

    reset_global_cooldown();
}

#[tokio::test]
async fn ack_gate_tracks_reset() {
    let _guard = test_support::registry_guard().await;
    test_support::reset(0);
    reset_global_cooldown();

    record_global_429(Duration::from_secs(5));
    assert!(!reaction_ack_permitted());

    reset_global_cooldown();
    assert!(
        reaction_ack_permitted(),
        "resetting the cooldown must immediately re-permit acks"
    );
}
