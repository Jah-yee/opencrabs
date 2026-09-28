//! #1776 regression pins: claude_cli system-channel task events.
//!
//! `CliMessage::System` used to be declared with ZERO fields, so serde
//! silently discarded the entire payload of every `{"type":"system",...}`
//! stdout line and the match arm reduced it to a debug log. A backgrounded
//! task's lifecycle (started, notification, roster change) never surfaced
//! anywhere after the turn's real events. These tests pin the two seams the
//! fix touches: serde field capture on the System variant, and the subtype
//! classification into `StreamEvent::BackgroundTask`.

use crate::brain::provider::claude_cli::{CliMessage, system_task_event};
use crate::brain::provider::types::StreamEvent;

/// Real payload shape from session logs (2026-09-27, task b48ojc8vb).
const TASK_STARTED: &str = r#"{"type":"system","subtype":"task_started","task_id":"b48ojc8vb","tool_use_id":"toolu_01Gk9N5U2H2dc879QL8Y1TTR","description":"Find Trello card and check push state","is_backgrounded":false,"task_type":"local_bash","uuid":"75de98e2-4a6b-4e06-8293-f635b81147a4","session_id":"ae4a6f3b-bbc6-4286-b3fc-012e1bab0f9a"}"#;

/// Real payload shape from session logs (2026-09-27, same task, +1.1s).
const TASK_NOTIFICATION: &str = r#"{"type":"system","subtype":"task_notification","task_id":"b48ojc8vb","tool_use_id":"toolu_01Gk9N5U2H2dc879QL8Y1TTR","status":"completed","output_file":"/private/tmp/claude-501/tasks/b48ojc8vb.output","summary":"Find Trello card and check push state","uuid":"8b3057b5-0e6a-4742-a22f-7619bba7c510","session_id":"ae4a6f3b-bbc6-4286-b3fc-012e1bab0f9a"}"#;

#[test]
fn system_variant_deserializes_task_started_payload() {
    // Regression: `System {}` had no fields, so serde swallowed these.
    let msg: CliMessage =
        serde_json::from_str(TASK_STARTED).expect("task_started system line must parse");
    match msg {
        CliMessage::System {
            subtype,
            task_id,
            status,
            description,
            summary,
            output_file,
        } => {
            assert_eq!(subtype.as_deref(), Some("task_started"));
            assert_eq!(task_id.as_deref(), Some("b48ojc8vb"));
            assert_eq!(
                description.as_deref(),
                Some("Find Trello card and check push state")
            );
            assert_eq!(summary, None);
            assert_eq!(status, None);
            assert_eq!(output_file, None);
        }
        other => panic!("expected System variant, got: {other:?}"),
    }
}

#[test]
fn system_variant_deserializes_task_notification_payload() {
    let msg: CliMessage =
        serde_json::from_str(TASK_NOTIFICATION).expect("task_notification system line must parse");
    match msg {
        CliMessage::System {
            subtype,
            task_id,
            status,
            description,
            summary,
            output_file,
        } => {
            assert_eq!(subtype.as_deref(), Some("task_notification"));
            assert_eq!(task_id.as_deref(), Some("b48ojc8vb"));
            assert_eq!(status.as_deref(), Some("completed"));
            assert_eq!(
                output_file.as_deref(),
                Some("/private/tmp/claude-501/tasks/b48ojc8vb.output")
            );
            // Notifications carry the human text as `summary`, not `description`.
            assert_eq!(description, None);
            assert_eq!(
                summary.as_deref(),
                Some("Find Trello card and check push state")
            );
        }
        other => panic!("expected System variant, got: {other:?}"),
    }
}

#[test]
fn init_system_payload_still_parses_without_task_fields() {
    // The same variant carries `init` and friends with disjoint shapes;
    // unknown fields must keep being ignored and the task fields stay None.
    let msg: CliMessage = serde_json::from_str(
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-4-8","tools":["Bash","Read"]}"#,
    )
    .expect("init system line must parse");
    match msg {
        CliMessage::System {
            subtype,
            task_id,
            status,
            description,
            summary,
            output_file,
        } => {
            assert_eq!(subtype.as_deref(), Some("init"));
            assert_eq!(task_id, None);
            assert_eq!(status, None);
            assert_eq!(description, None);
            assert_eq!(summary, None);
            assert_eq!(output_file, None);
        }
        other => panic!("expected System variant, got: {other:?}"),
    }
}

#[test]
fn classifier_surfaces_all_three_task_subtypes_with_fields() {
    for subtype in [
        "task_started",
        "task_notification",
        "background_tasks_changed",
    ] {
        let ev = system_task_event(
            Some(subtype),
            Some("t1".to_string()),
            Some("completed".to_string()),
            Some("desc".to_string()),
            None,
            Some("/tmp/o".to_string()),
        );
        match ev {
            Some(StreamEvent::BackgroundTask {
                subtype: s,
                task_id,
                status,
                description,
                output_file,
            }) => {
                assert_eq!(s, subtype);
                assert_eq!(task_id.as_deref(), Some("t1"));
                assert_eq!(status.as_deref(), Some("completed"));
                assert_eq!(description.as_deref(), Some("desc"));
                assert_eq!(output_file.as_deref(), Some("/tmp/o"));
            }
            other => panic!("expected BackgroundTask for {subtype}, got: {other:?}"),
        }
    }
}

#[test]
fn classifier_ignores_non_task_subtypes_and_missing_subtype() {
    // Non-task subtypes keep the debug-only path (None), and a system line
    // with no subtype at all must not panic or surface.
    assert!(system_task_event(Some("init"), None, None, None, None, None).is_none());
    assert!(system_task_event(Some("other_tool"), None, None, None, None, None).is_none());
    assert!(system_task_event(None, Some("t1".to_string()), None, None, None, None).is_none());
}

#[test]
fn classifier_falls_back_to_summary_when_description_absent() {
    // `task_notification` ships the human text as `summary`; the surfaced
    // event must carry it in `description` either way.
    let ev = system_task_event(
        Some("task_notification"),
        Some("t2".to_string()),
        Some("completed".to_string()),
        None,
        Some("summary text".to_string()),
        None,
    );
    match ev {
        Some(StreamEvent::BackgroundTask {
            description,
            task_id,
            ..
        }) => {
            assert_eq!(description.as_deref(), Some("summary text"));
            assert_eq!(task_id.as_deref(), Some("t2"));
        }
        other => panic!("expected BackgroundTask, got: {other:?}"),
    }
    // An explicit description wins over summary.
    let ev = system_task_event(
        Some("task_started"),
        Some("t3".to_string()),
        None,
        Some("explicit".to_string()),
        Some("ignored".to_string()),
        None,
    );
    match ev {
        Some(StreamEvent::BackgroundTask { description, .. }) => {
            assert_eq!(description.as_deref(), Some("explicit"));
        }
        other => panic!("expected BackgroundTask, got: {other:?}"),
    }
}
