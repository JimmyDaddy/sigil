use anyhow::Result;
use sigil_kernel::{ControlEntry, JsonlSessionStore, ModelMessage, Session};

use super::*;

fn write_config(path: &Path, compaction_enabled: bool) -> Result<()> {
    std::fs::write(
        path,
        format!(
            r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[compaction]
enabled = {compaction_enabled}
tail_messages = 2
"#,
        ),
    )?;
    Ok(())
}

fn session_with_messages(path: &Path, messages: &[&str]) -> Result<String> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    })?;
    for message in messages {
        session.append_user_message(ModelMessage::user(*message))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("reply to {message}")),
            Vec::new(),
        ))?;
    }
    Ok(session.session_scope_id().to_owned())
}

#[tokio::test]
async fn preview_reports_no_foldable_history_without_creating_lifecycle_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    write_config(&config_path, true)?;
    let scope = session_with_messages(&session_path, &["hello"])?;
    let before = std::fs::read(&session_path)?;

    let (review, pending) =
        prepare_application_compaction(&config_path, temp.path(), &session_path, &scope).await?;

    assert!(pending.is_none());
    assert!(review.preview_id.is_none());
    assert!(matches!(
        review.admission,
        ApplicationCompactionAdmission::NoFoldableHistory {
            durable_message_count: 2,
            configured_tail_message_count: 2,
        }
    ));
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[tokio::test]
async fn preview_preserves_disabled_and_scope_failure_semantics() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    write_config(&config_path, false)?;
    let scope = session_with_messages(&session_path, &["one", "two"])?;

    let (review, pending) =
        prepare_application_compaction(&config_path, temp.path(), &session_path, &scope).await?;
    assert!(pending.is_none());
    assert!(matches!(
        review.admission,
        ApplicationCompactionAdmission::Unavailable { ref reason }
            if reason.contains("disabled")
    ));
    assert!(
        prepare_application_compaction(&config_path, temp.path(), &session_path, "another-scope",)
            .await
            .is_err()
    );
    Ok(())
}
