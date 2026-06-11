use std::path::Path;

use anyhow::Result;
use sigil_kernel::{
    CompactionConfig, CompactionRecord, CompactionThresholdStatus, JsonlSessionStore, Session,
};

use super::protocol::{CompactionTrigger, WorkerMessage};

pub(super) fn load_session(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
) -> Result<Session> {
    let store = JsonlSessionStore::new(session_log_path)?;
    Session::load_from_store(provider_name.to_owned(), model_name.to_owned(), store)
}

pub(super) fn auto_compact_session(
    session: &mut Session,
    config: &CompactionConfig,
) -> Result<Option<CompactionRecord>> {
    if config.threshold_status(session.stats().last_prompt_tokens)
        != CompactionThresholdStatus::Hard
    {
        return Ok(None);
    }
    if !session.can_compact(config) {
        return Ok(None);
    }

    session.compact_now(config).map(Some)
}

pub(super) fn session_compacted_message(
    session_log_path: &Path,
    session: &Session,
    record: CompactionRecord,
    trigger: CompactionTrigger,
) -> WorkerMessage {
    WorkerMessage::SessionCompacted {
        session_log_path: session_log_path.to_path_buf(),
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
        record,
        trigger,
        entries: session.entries().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sigil_kernel::{
        CompactionConfig, CompactionRecord, ModelMessage, Session, SessionLogEntry,
    };

    use super::*;

    fn test_compaction_config(hard_ratio: f32) -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            context_window_tokens: Some(100),
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: hard_ratio,
            tail_messages: 2,
        }
    }

    #[test]
    fn load_session_fails_for_missing_log() {
        let result = load_session("p", "m", &PathBuf::from("/nonexistent/path/session.jsonl"));
        assert!(result.is_err());
    }

    #[test]
    fn auto_compact_returns_none_for_disabled_config() -> Result<()> {
        let mut session = Session::new("test-provider".to_owned(), "test-model".to_owned());
        let config = CompactionConfig {
            enabled: false,
            ..test_compaction_config(0.8)
        };
        let result = auto_compact_session(&mut session, &config)?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn auto_compact_returns_none_below_hard_threshold() -> Result<()> {
        let mut session = Session::new("test-provider".to_owned(), "test-model".to_owned());
        let config = test_compaction_config(0.8); // hard at 80 tokens
        // Session stats default to 0 tokens, well below hard threshold
        let result = auto_compact_session(&mut session, &config)?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn auto_compact_returns_none_when_cannot_compact() -> Result<()> {
        let session = Session::new("test-provider".to_owned(), "test-model".to_owned());
        // Session is empty, can_compact should return false
        let mut session2 = session;
        // Force stats to exceed hard threshold
        session2.stats_mut().apply_usage(&sigil_kernel::UsageStats {
            prompt_tokens: 90,
            ..Default::default()
        });
        let config = test_compaction_config(0.8); // hard at 80
        // But session has too few messages to compact
        let result = auto_compact_session(&mut session2, &config)?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn session_compacted_message_includes_provider_and_model() -> Result<()> {
        let session = Session::new("deepseek".to_owned(), "deepseek-v4-flash".to_owned());
        let path = PathBuf::from("/tmp/test-session.jsonl");
        let record = CompactionRecord {
            summary: "test summary".to_owned(),
            compacted_message_count: 5,
            retained_tail_message_count: 2,
        };
        let message =
            session_compacted_message(&path, &session, record.clone(), CompactionTrigger::Manual);
        match message {
            WorkerMessage::SessionCompacted {
                session_log_path,
                provider_name,
                model_name,
                record: returned_record,
                trigger,
                ..
            } => {
                assert_eq!(session_log_path, path);
                assert_eq!(provider_name, "deepseek");
                assert_eq!(model_name, "deepseek-v4-flash");
                assert_eq!(returned_record, record);
                assert_eq!(trigger, CompactionTrigger::Manual);
                Ok(())
            }
            _ => panic!("expected SessionCompacted message"),
        }
    }

    #[test]
    fn session_compacted_message_captures_entries() -> Result<()> {
        let mut session = Session::new("p".to_owned(), "m".to_owned());
        session.append_user_message(ModelMessage::user("test prompt"))?;
        let path = PathBuf::from("/tmp/test-entries.jsonl");
        let record = CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 0,
            retained_tail_message_count: 0,
        };
        let message = session_compacted_message(
            &path,
            &session,
            record,
            CompactionTrigger::AutomaticHardThreshold,
        );
        match message {
            WorkerMessage::SessionCompacted {
                entries, trigger, ..
            } => {
                assert_eq!(trigger, CompactionTrigger::AutomaticHardThreshold);
                assert!(entries
                    .iter()
                    .any(|e| matches!(e, SessionLogEntry::User(m) if m.content.as_deref() == Some("test prompt"))));
                Ok(())
            }
            _ => panic!("expected SessionCompacted"),
        }
    }

    #[test]
    fn session_compacted_message_uses_trigger_enum() {
        let session = Session::new("p".to_owned(), "m".to_owned());
        let path = PathBuf::from("/tmp/test-trigger.jsonl");
        let record = CompactionRecord {
            summary: "s".to_owned(),
            compacted_message_count: 0,
            retained_tail_message_count: 0,
        };

        let manual =
            session_compacted_message(&path, &session, record.clone(), CompactionTrigger::Manual);
        let auto = session_compacted_message(
            &path,
            &session,
            record,
            CompactionTrigger::AutomaticHardThreshold,
        );

        assert!(matches!(
            manual,
            WorkerMessage::SessionCompacted {
                trigger: CompactionTrigger::Manual,
                ..
            }
        ));
        assert!(matches!(
            auto,
            WorkerMessage::SessionCompacted {
                trigger: CompactionTrigger::AutomaticHardThreshold,
                ..
            }
        ));
    }
}
