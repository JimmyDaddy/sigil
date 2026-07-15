use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use serde_json::Value;

use super::*;

#[derive(Default)]
struct RecordingRegistrar {
    staged: Mutex<Vec<UserUrlCapabilityRegistration>>,
    committed: Mutex<Vec<String>>,
    rolled_back: Mutex<Vec<String>>,
    fail_stage: bool,
}

impl UserUrlCapabilityRegistrar for RecordingRegistrar {
    fn stage(&self, registration: UserUrlCapabilityRegistration) -> Result<()> {
        if self.fail_stage {
            bail!("injected stage failure");
        }
        self.staged
            .lock()
            .map_err(|_| anyhow::anyhow!("staged lock poisoned"))?
            .push(registration);
        Ok(())
    }

    fn commit_message(&self, durable_entry_id: &str) -> Result<()> {
        self.committed
            .lock()
            .map_err(|_| anyhow::anyhow!("commit lock poisoned"))?
            .push(durable_entry_id.to_owned());
        Ok(())
    }

    fn rollback_message(&self, durable_entry_id: &str) -> Result<()> {
        self.rolled_back
            .lock()
            .map_err(|_| anyhow::anyhow!("rollback lock poisoned"))?
            .push(durable_entry_id.to_owned());
        Ok(())
    }
}

#[test]
fn safe_persistence_user_projection_masks_query_and_keeps_exact_overlay() -> Result<()> {
    let registrar = Arc::new(RecordingRegistrar::default());
    let registrar_trait: Arc<dyn UserUrlCapabilityRegistrar> = registrar.clone();
    let raw = "inspect https://example.com/report?token=known-secret&sig=abc now";
    let projection =
        project_user_message_for_persistence("user-entry-1", raw, Some(&registrar_trait))?;

    let durable = projection
        .durable_message
        .content
        .as_deref()
        .unwrap_or_default();
    assert!(!durable.contains("known-secret"));
    assert!(!durable.contains("token="));
    assert_eq!(projection.capability_registrations.len(), 1);
    let staged = registrar
        .staged
        .lock()
        .map_err(|_| anyhow::anyhow!("staged lock poisoned"))?;
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].durable_entry_id, "user-entry-1");
    assert_eq!(
        staged[0].raw_canonical_url.expose_secret(),
        "https://example.com/report?token=known-secret&sig=abc"
    );

    let request = apply_exact_message_overlays(
        std::slice::from_ref(&projection.durable_message),
        std::slice::from_ref(&projection.overlay),
    )?;
    assert_eq!(request.len(), 1);
    assert_eq!(request[0].content.as_deref(), Some(raw));
    Ok(())
}

#[test]
fn safe_persistence_sensitive_and_percent_encoded_paths_are_not_replayable() -> Result<()> {
    for (index, raw) in [
        "download https://example.com/files/known-secret-token",
        "download https://example.com/files/%73ecret-credential",
        "download https://example.com/files/Az09bcdefghijklmnopqrstuvwx",
    ]
    .into_iter()
    .enumerate()
    {
        let projection =
            project_user_message_for_persistence(format!("sensitive-path-{index}"), raw, None)?;
        let durable = projection
            .durable_message
            .content
            .as_deref()
            .unwrap_or_default();
        assert!(!durable.contains("known-secret-token"));
        assert!(!durable.contains("%73ecret-credential"));
        assert!(!durable.contains("Az09bcdefghijklmnopqrstuvwx"));
        let registration = projection
            .capability_registrations
            .first()
            .ok_or_else(|| anyhow::anyhow!("URL registration missing"))?;
        assert_eq!(
            registration.restart_policy,
            ToolRestartPolicy::InterruptOnRestart
        );
        assert!(registration.replayable_canonical_url.is_none());
        assert_eq!(
            registration.safe_display_url,
            "https://example.com/[redacted]"
        );
    }
    Ok(())
}

#[test]
fn safe_persistence_queryless_public_url_has_explicit_replayable_value() -> Result<()> {
    let projection = project_user_message_for_persistence(
        "public-path",
        "read https://example.com/public/report.html",
        None,
    )?;
    let registration = projection
        .capability_registrations
        .first()
        .ok_or_else(|| anyhow::anyhow!("URL registration missing"))?;
    assert_eq!(registration.restart_policy, ToolRestartPolicy::Replayable);
    assert_eq!(
        registration.replayable_canonical_url.as_deref(),
        Some("https://example.com/public/report.html")
    );
    let descriptor = registration.durable_descriptor("session-1");
    descriptor.validate()?;
    assert_eq!(
        descriptor.safe_display_url,
        descriptor
            .replayable_canonical_url
            .as_deref()
            .unwrap_or_default()
    );
    Ok(())
}

#[test]
fn safe_persistence_overlay_requires_exactly_one_durable_identity() -> Result<()> {
    let projection = project_user_message_for_persistence("entry", "safe", None)?;
    let error = apply_exact_message_overlays(&[], &[projection.overlay])
        .expect_err("missing durable entry must fail closed");
    assert!(matches!(
        error,
        SafePersistenceError::OverlayInvariant { .. }
    ));
    Ok(())
}

#[test]
fn safe_persistence_stage_failure_rolls_back_before_append() {
    let registrar = Arc::new(RecordingRegistrar {
        fail_stage: true,
        ..RecordingRegistrar::default()
    });
    let registrar_trait: Arc<dyn UserUrlCapabilityRegistrar> = registrar.clone();
    let error = project_user_message_for_persistence(
        "entry-fail",
        "https://example.com/?secret=value",
        Some(&registrar_trait),
    )
    .expect_err("stage failure must fail projection");
    assert!(format!("{error:#}").contains("stage failure"));
    assert_eq!(
        registrar
            .rolled_back
            .lock()
            .map(|values| values.clone())
            .unwrap_or_default(),
        vec!["entry-fail".to_owned()]
    );
}

#[test]
fn safe_persistence_tool_projection_redacts_secret_and_sensitive_url() -> Result<()> {
    let projection = project_tool_call_for_persistence(ToolCall {
        id: "call-1".to_owned(),
        name: "webfetch".to_owned(),
        args_json: serde_json::json!({
            "url": "https://example.com/x?signature=known-secret",
            "api_key": "known-secret",
            "format": "markdown",
        })
        .to_string(),
    })?;
    assert!(
        projection
            .clone()
            .into_exact_call()
            .args_json
            .contains("known-secret")
    );
    assert!(!projection.durable_call.args_json.contains("known-secret"));
    let safe: Value = serde_json::from_str(&projection.durable_call.args_json)?;
    assert_eq!(safe["api_key"], "[redacted]");
    assert!(
        safe["url"]
            .as_str()
            .is_some_and(|url| url.contains("[redacted]"))
    );
    Ok(())
}

#[test]
fn safe_persistence_preserves_exact_whitespace_while_redacting_carriers() {
    let raw = "\nfirst line\n\nsecond  token=secret-value\t--password\nsecret-two";
    let safe = safe_persistence_text(raw);

    assert_eq!(
        safe,
        "\nfirst line\n\nsecond  token=[redacted]\t--password\n[redacted]"
    );
}

#[test]
fn safe_persistence_json_recursively_redacts_secret_keys_and_url_queries() {
    let safe = safe_persistence_json_value(serde_json::json!({
        "nested": [{
            "api_key": "super-secret",
            "destination": "https://example.test/path?token=super-secret",
        }],
        "ordinary": true,
    }));

    assert_eq!(safe["nested"][0]["api_key"], "[redacted]");
    assert!(
        safe["nested"][0]["destination"]
            .as_str()
            .is_some_and(|value| !value.contains("super-secret") && !value.contains("?token="))
    );
    assert_eq!(safe["ordinary"], true);
}

#[test]
fn safe_persistence_one_shot_tool_call_complete_is_capped() {
    let error = project_tool_call_for_persistence(ToolCall {
        id: "call-large".to_owned(),
        name: "unknown".to_owned(),
        args_json: "x".repeat(MAX_STREAMED_TOOL_ARGS_BYTES + 1),
    })
    .expect_err("oversized one-shot call must fail closed");
    assert!(matches!(
        error,
        SafePersistenceError::ToolArgsTooLarge { .. }
    ));
}

#[test]
fn safe_persistence_rejects_secret_bearing_tool_id_and_name_before_projection() {
    for call in [
        ToolCall {
            id: "https://example.com/call?token=known-secret".to_owned(),
            name: "webfetch".to_owned(),
            args_json: "{}".to_owned(),
        },
        ToolCall {
            id: "call-safe".to_owned(),
            name: "webfetch?token=known-secret".to_owned(),
            args_json: "{}".to_owned(),
        },
        ToolCall {
            id: "authorization:knownsecret".to_owned(),
            name: "webfetch".to_owned(),
            args_json: "{}".to_owned(),
        },
        ToolCall {
            id: "secret-token-abc".to_owned(),
            name: "webfetch".to_owned(),
            args_json: "{}".to_owned(),
        },
    ] {
        let error = project_tool_call_for_persistence(call)
            .expect_err("unsafe identity must fail before durable/event projection");
        assert!(matches!(
            error,
            SafePersistenceError::ToolCallIdentityUnsafe { .. }
        ));
    }
}

#[test]
fn streamed_tool_identity_failure_emits_only_redacted_terminal_error() {
    let secret = "known-secret";
    let mut accumulator = crate::ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();
    accumulator.append_delta(
        &mut chunks,
        0,
        Some(format!("https://example.com/?token={secret}")),
        Some("webfetch".to_owned()),
        Some(format!(r#"{{"token":"{secret}"}}"#)),
    );

    assert_eq!(chunks.len(), 1);
    assert!(matches!(
        chunks[0],
        crate::ProviderChunk::ToolCallStreamError(SafePersistenceError::ToolCallIdentityUnsafe {
            field: "id"
        })
    ));
    assert!(!format!("{:?}", chunks[0]).contains(secret));
}
