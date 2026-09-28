//! Text-directive recovery for options the model wrote as TEXT instead of
//! invoking `suggest_options` (#1774).
//!
//! When the model fails to emit the structured tool call, it sometimes
//! imitates the marker style taught in prompts (`<<react:...>>`) and writes a
//! `<<suggest_options>>` marker pair around a JSON options array into its
//! final text. The residue then ships to channels as raw JSON with dead
//! markers: buttons never render because the tool never ran.
//!
//! [`extract_leaked_suggestions`] recovers the block: the array is parsed
//! into real `SuggestionItem`s (so every surface's native renderer fires via
//! `ProgressEvent::SuggestedOptions`) and the whole block is stripped from
//! the delivered text. A marker block whose payload does not parse is
//! stripped too: raw `<<...>>` markers and raw JSON never reach a channel.

use crate::brain::tools::suggest_options::{RawSuggestionItem, SuggestionItem, MAX_OPTIONS};

const MARKER: &str = "<<suggest_options>>";

/// Recover a leaked `<<suggest_options>>` block from final-response text.
///
/// Returns `(cleaned_text, Some(items))` when a marker pair held a parseable,
/// non-empty options array, and `(cleaned_text, None)` in every other case
/// (no markers, unclosed marker, unparseable or empty payload). The cleaned
/// text never contains the marker token; surrounding prose is preserved with
/// a blank line at the seam.
pub fn extract_leaked_suggestions(text: &str) -> (String, Option<Vec<SuggestionItem>>) {
    let Some(start) = text.find(MARKER) else {
        return (text.to_string(), None);
    };
    let inner_start = start + MARKER.len();
    let Some(rel_end) = text[inner_start..].find(MARKER) else {
        // Unclosed marker: drop the stray marker token, keep the text.
        let mut cleaned = String::with_capacity(text.len());
        cleaned.push_str(&text[..start]);
        cleaned.push_str(&text[inner_start..]);
        return (cleaned, None);
    };
    let tail_start = inner_start + rel_end + MARKER.len();
    let inner = text[inner_start..inner_start + rel_end].trim();

    let parsed = serde_json::from_str::<Vec<RawSuggestionItem>>(inner)
        .ok()
        .filter(|items| !items.is_empty())
        .map(|items| {
            items
                .into_iter()
                .take(MAX_OPTIONS)
                .map(RawSuggestionItem::into_suggestion_item)
                .collect::<Vec<SuggestionItem>>()
        });

    let head = text[..start].trim_end();
    let tail = text[tail_start..].trim_start();
    let mut cleaned = String::with_capacity(text.len());
    cleaned.push_str(head);
    if !head.is_empty() && !tail.is_empty() {
        cleaned.push_str("\n\n");
    }
    cleaned.push_str(tail);
    (cleaned, parsed)
}
