//! Tests for `utils::directives::extract_leaked_suggestions` — the #1774
//! recovery for `<<suggest_options>>` blocks the model wrote as TEXT instead
//! of invoking the tool.
//!
//! The incident: the marker pair around a JSON options array shipped to a
//! channel verbatim (dead markers + raw JSON, buttons never rendered because
//! the tool never ran). The extractor parses the array into real
//! `SuggestionItem`s so every surface's native renderer fires, and strips the
//! block from the delivered text. A block whose payload does not parse loses
//! its markers and payload too: raw markers and raw JSON never reach a
//! channel. Payload shapes verbatim from the #1774 issue capture.

use crate::brain::tools::suggest_options::{MAX_OPTIONS, SuggestionStyle};
use crate::utils::directives::extract_leaked_suggestions;

// ── recovery: parseable blocks ───────────────────────────────────────────

#[test]
fn recovers_styled_array_from_marker_pair() {
    // Verbatim shape from the #1774 channel capture.
    let text = "Next step? <<suggest_options>>\n\
                [{\"label\":\"Run the gates\",\"style\":\"primary\"},\
                {\"label\":\"Park it\"}]\n\
                <<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert_eq!(cleaned, "Next step?");
    let items = items.expect("styled array must recover");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "Run the gates");
    assert_eq!(items[0].style, SuggestionStyle::Primary);
    assert_eq!(items[1].label, "Park it");
    assert_eq!(items[1].style, SuggestionStyle::Default);
}

#[test]
fn recovers_bare_string_items() {
    let text = "<<suggest_options>>[\"Go\", \"Hold on\"]<<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert_eq!(cleaned, "");
    let items = items.expect("bare strings must recover");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "Go");
    assert_eq!(items[1].label, "Hold on");
}

#[test]
fn recovers_pretty_printed_array() {
    let text = "<<suggest_options>>\n[\n  {\"label\": \"A\"},\n  \"B\"\n]\n<<suggest_options>>";
    let (_, items) = extract_leaked_suggestions(text);
    let items = items.expect("pretty-printed array must recover");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "A");
    assert_eq!(items[1].label, "B");
}

#[test]
fn caps_at_max_options() {
    let payload: Vec<String> = (0..MAX_OPTIONS + 5).map(|i| format!("opt{i}")).collect();
    let text = format!("<<suggest_options>>[{}]<<suggest_options>>", {
        let quoted: Vec<String> = payload.iter().map(|s| format!("\"{s}\"")).collect();
        quoted.join(",")
    });
    let (_, items) = extract_leaked_suggestions(&text);
    let items = items.expect("oversized array still recovers (capped)");
    assert_eq!(items.len(), MAX_OPTIONS);
}

// ── text preservation ────────────────────────────────────────────────────

#[test]
fn preserves_surrounding_prose_with_clean_seam() {
    let text = "Intro line.\n<<suggest_options>>[\"A\"]<<suggest_options>>\nClosing line.";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert!(items.is_some());
    assert_eq!(cleaned, "Intro line.\n\nClosing line.");
}

#[test]
fn leaves_text_without_markers_untouched() {
    let text = "Plain prose with [\"not\", \"options\"] inside.";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert_eq!(cleaned, text);
    assert!(items.is_none());
}

// ── degradation: never ship markers or raw JSON ──────────────────────────

#[test]
fn strips_unparseable_block_entirely() {
    let text = "Answer.\n<<suggest_options>>[{\"label\": broken<<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert!(items.is_none());
    assert_eq!(cleaned, "Answer.");
    assert!(!cleaned.contains("suggest_options"));
    assert!(!cleaned.contains("broken"));
}

#[test]
fn strips_empty_array_block() {
    let text = "<<suggest_options>>[]<<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert!(items.is_none());
    assert_eq!(cleaned, "");
}

#[test]
fn drops_unclosed_marker_token() {
    let text = "Working on it. <<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert!(items.is_none());
    assert_eq!(cleaned, "Working on it. ");
    assert!(!cleaned.contains("suggest_options"));
}

#[test]
fn object_without_label_is_not_recoverable() {
    // RawSuggestionItem requires a string or {label}: an alien object array
    // must NOT become suggestions, and must not ship as markers.
    let text = "<<suggest_options>>[{\"command\": \"ls\"}]<<suggest_options>>";
    let (cleaned, items) = extract_leaked_suggestions(text);
    assert!(items.is_none());
    assert_eq!(cleaned, "");
}
