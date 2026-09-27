//! Structural pins for the Ctrl+C documentation drift (#1770, #1771).
//!
//! The real behavior lives in `src/tui/app/state.rs`: when the transcript is
//! scrolled up (`!auto_scroll`), the first Ctrl+C press snaps back to bottom
//! and clears the input — no quit, no quit hint. Both documented surfaces
//! (README shortcut table, `/help` dialog) described only clear-input/quit,
//! lying by omission. Source scans keep the docs welded to the behavior.

const README: &str = include_str!("../../README.md");

#[test]
fn readme_ctrl_c_row_documents_snap_to_bottom() {
    let row = README
        .lines()
        .find(|l| l.trim_start().starts_with("| `Ctrl+C` |"))
        .expect("README must keep a `Ctrl+C` row in the Keyboard Shortcuts table");
    assert!(
        row.contains("bottom") && row.contains("scrolled"),
        "#1770: the Ctrl+C row must document that a scrolled-up transcript \
         snaps to bottom on the first press, not only clear-input/quit. \
         Offending row: {row}"
    );
    assert!(
        row.contains("quit"),
        "#1770: the Ctrl+C row must keep the double-press quit behavior too. \
         Offending row: {row}"
    );
}
