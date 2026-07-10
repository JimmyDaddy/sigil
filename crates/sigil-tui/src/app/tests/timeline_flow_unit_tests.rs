use super::agent_thread_status_label;
use sigil_kernel::{AgentInvocationSource, AgentThreadStatus};

fn agent_thread_source_label(source: Option<AgentInvocationSource>) -> &'static str {
    match source {
        Some(AgentInvocationSource::Chat) => "chat",
        Some(AgentInvocationSource::Mention) => "mention",
        Some(AgentInvocationSource::Skill) => "skill",
        Some(AgentInvocationSource::Task) => "task",
        Some(AgentInvocationSource::Plugin) => "plugin",
        Some(AgentInvocationSource::System) => "system",
        Some(AgentInvocationSource::Unknown) | None => "unknown",
    }
}

#[test]
fn agent_thread_labels_cover_status_and_source_variants() {
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Started),
        "started"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Running),
        "running"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Completed),
        "completed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Failed),
        "failed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Interrupted),
        "interrupted"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Closed),
        "closed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Unavailable),
        "unavailable"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Chat)),
        "chat"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Mention)),
        "mention"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Skill)),
        "skill"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Task)),
        "task"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Plugin)),
        "plugin"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::System)),
        "system"
    );
    assert_eq!(
        agent_thread_source_label(Some(AgentInvocationSource::Unknown)),
        "unknown"
    );
    assert_eq!(agent_thread_source_label(None), "unknown");
}
