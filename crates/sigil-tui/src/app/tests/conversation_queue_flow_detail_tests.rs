use std::path::Path;

use super::*;
use crate::app::tests::common::test_config;

fn queued_item(
    id: &str,
    kind: sigil_kernel::ConversationInputKind,
    status: sigil_kernel::ConversationInputStatus,
) -> sigil_kernel::ConversationQueueItemProjection {
    sigil_kernel::ConversationQueueItemProjection {
        queued: sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new(id).expect("valid queue id"),
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind,
            prompt_hash: format!("sha256:{id}"),
            prompt: format!("{id} prompt"),
            reasoning_effort: None,
            created_at_ms: None,
        },
        status,
        reason: None,
    }
}

fn queued_entry(id: &str, prompt: &str) -> sigil_kernel::SessionLogEntry {
    sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::ConversationInputQueued(
        sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new(id).expect("valid queue id"),
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: format!("sha256:{id}"),
            prompt: prompt.to_owned(),
            reasoning_effort: None,
            created_at_ms: None,
        },
    ))
}

#[test]
fn queue_flow_helpers_cover_kinds_statuses_and_empty_targets() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_entry("queue_1", "first"),
        queued_entry("queue_2", "second"),
    ]);
    assert_eq!(
        app.queue_index_for_target("")
            .expect("empty target uses selection"),
        0
    );
    app.composer.queue_selected = 1;
    let selected = app
        .queue_action_for_target("", |queue_id| AppAction::PromoteQueuedConversationInput {
            queue_id,
        })
        .expect("selected queue item should resolve");
    assert!(matches!(
        selected,
        AppAction::PromoteQueuedConversationInput { ref queue_id }
            if queue_id.as_str() == "queue_2"
    ));

    let paused = queued_item(
        "queue_paused",
        sigil_kernel::ConversationInputKind::Chat,
        sigil_kernel::ConversationInputStatus::Queued,
    );
    assert_eq!(queue_item_detail(&paused, true), "paused · chat");
    assert_eq!(
        queue_status_kind(sigil_kernel::ConversationInputStatus::Queued, true),
        StatusKind::Warning
    );

    for (kind, label) in [
        (
            sigil_kernel::ConversationInputKind::PlanPrompt,
            "queued · plan",
        ),
        (
            sigil_kernel::ConversationInputKind::AgentMention,
            "queued · agent",
        ),
        (
            sigil_kernel::ConversationInputKind::AgentMessage,
            "queued · message",
        ),
        (
            sigil_kernel::ConversationInputKind::Unknown,
            "queued · unknown",
        ),
    ] {
        assert_eq!(
            queue_item_detail(
                &queued_item(
                    "queue_kind",
                    kind,
                    sigil_kernel::ConversationInputStatus::Queued
                ),
                false,
            ),
            label
        );
    }

    for (status, label, kind) in [
        (
            sigil_kernel::ConversationInputStatus::Dispatching,
            "dispatching",
            StatusKind::Running,
        ),
        (
            sigil_kernel::ConversationInputStatus::Delivered,
            "delivered",
            StatusKind::Success,
        ),
        (
            sigil_kernel::ConversationInputStatus::Rejected,
            "rejected",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Cancelled,
            "cancelled",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Stale,
            "stale",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Unknown,
            "unknown",
            StatusKind::Unknown,
        ),
    ] {
        assert_eq!(queue_status_label(status), label);
        assert_eq!(queue_status_kind(status, false), kind);
    }
}
