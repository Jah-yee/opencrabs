use crate::brain::provider::custom_openai_compatible::stable_model_id;

// Regression tests for #1756: gateways on the OpenAI-compatible surface may
// return a backend-local model id (provider prefix stripped). The requested
// id is what the session pair and the next turn's routing need, so it wins
// whenever the request carried one.

#[test]
fn preserves_provider_prefix_when_gateway_strips_it() {
    assert_eq!(
        stable_model_id("gpt-5.6-luna-medium".into(), "codex/gpt-5.6-luna-medium"),
        "codex/gpt-5.6-luna-medium"
    );
}

#[test]
fn falls_back_to_response_model_without_request_id() {
    assert_eq!(stable_model_id("backend-model".into(), ""), "backend-model");
}
