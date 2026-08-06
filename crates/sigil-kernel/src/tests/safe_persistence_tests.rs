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
fn safe_persistence_image_projection_keeps_metadata_and_exact_bytes_only_in_overlay() -> Result<()>
{
    let attachment = crate::ImageAttachment::from_bytes(
        "image-1",
        crate::ImageMimeType::Png,
        1,
        1,
        vec![1, 2, 3],
    )?;
    let projection =
        project_user_message_with_attachments_for_persistence_with_nonce_and_issued_at(
            "user-image-1",
            "inspect this",
            vec![attachment],
            None,
            1,
            None,
        )?;

    assert!(!projection.durable_message.image_attachments[0].has_resolved_bytes());
    assert!(
        projection
            .durable_message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("[Image attachment 1:"))
    );
    let durable_json = serde_json::to_string(&projection.durable_message)?;
    assert!(!durable_json.contains("AQID"));

    let exact = apply_exact_message_overlays(
        std::slice::from_ref(&projection.durable_message),
        std::slice::from_ref(&projection.overlay),
    )?;
    assert_eq!(exact[0].image_attachments[0].resolved_bytes()?, &[1, 2, 3]);
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
fn safe_persistence_url_query_projection_is_idempotent() {
    let raw = "read https://example.test/report?token=super-secret now";
    let canonical = "read https://example.test/report?[redacted] now";

    assert_eq!(safe_persistence_text(raw), canonical);
    assert_eq!(safe_persistence_text(canonical), canonical);
    assert_eq!(
        safe_persistence_text(
            "read https://example.test/report?[redacted][redacted][redacted] now"
        ),
        canonical
    );
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

/// Mirrors the `finish_process_capture` pipeline: per-stream projection, then the boundary pass.
fn redact_cross_stream_pair(stdout: &str, stderr: &str) -> (String, String) {
    let stdout = safe_persistence_text(stdout);
    let stderr = safe_persistence_text(stderr);
    redact_cross_stream_boundary(&stdout, &stderr)
}

fn assert_cross_stream_closed(stdout: &str, stderr: &str, forbidden: &[&str]) {
    let (safe_stdout, safe_stderr) = redact_cross_stream_pair(stdout, stderr);
    let canonical = format!("{safe_stdout}{safe_stderr}");
    for secret in forbidden {
        assert!(
            !canonical.contains(secret),
            "canonical must not contain {secret:?}, got: {canonical:?}"
        );
    }
    assert_eq!(
        safe_persistence_text(&canonical),
        canonical,
        "canonical must be redaction-closed, got: {canonical:?}"
    );
}

#[test]
fn cross_stream_boundary_redacts_value_markers_at_every_split() {
    // RFC-0062 16.1: for every supported value-marker secret, split the full match at every
    // possible byte boundary between stdout and stderr; the canonical body must never reassemble
    // the match and must stay redaction-closed.
    for match_text in [
        "token=SUPER-SECRET-123",
        "secret=SUPER-SECRET-123",
        "password=SUPER-SECRET-123",
        "api_key=SUPER-SECRET-123",
        "apikey=SUPER-SECRET-123",
        "authorization=SUPER-SECRET-123",
    ] {
        for split in 0..=match_text.len() {
            let stdout = format!("head {}", &match_text[..split]);
            let stderr = format!("{} tail", &match_text[split..]);
            assert_cross_stream_closed(&stdout, &stderr, &[match_text]);
        }
    }
}

#[test]
fn cross_stream_boundary_redacts_carry_markers_at_every_split() {
    // The whitespace-separated carry forms (--token VALUE etc.) must hold at every split too:
    // the merged token may itself become the carry marker, and the value token on the stderr
    // side must then be redacted.
    for match_text in [
        "--token SUPER-SECRET-123",
        "--secret SUPER-SECRET-123",
        "--password SUPER-SECRET-123",
        "--api-key SUPER-SECRET-123",
    ] {
        for split in 0..=match_text.len() {
            let stdout = format!("head {}", &match_text[..split]);
            let stderr = format!("{} tail", &match_text[split..]);
            assert_cross_stream_closed(&stdout, &stderr, &[match_text]);
        }
    }
}

#[test]
fn cross_stream_boundary_redacts_url_queries_split_across_streams() {
    let url = "https://example.com/path?token=SUPER-SECRET-123";
    for split in 0..=url.len() {
        let stdout = format!("head {}", &url[..split]);
        let stderr = format!("{} tail", &url[split..]);
        assert_cross_stream_closed(&stdout, &stderr, &[url, "token=SUPER-SECRET-123"]);
    }
}

#[test]
fn cross_stream_boundary_attributes_replacements_to_the_span_start_stream() {
    // Marker split at the boundary: the whole replacement (including the stderr value bytes it
    // replaces) is attributed to stdout; the stderr segment loses the absorbed bytes.
    let (stdout, stderr) = redact_cross_stream_pair("head tok", "en=x tail");
    assert_eq!(stdout, "head token=[redacted]");
    assert_eq!(stderr, " tail");

    // Expansion: token=x -> token=[redacted] grows the stdout segment.
    let (stdout, stderr) = redact_cross_stream_pair("tok", "en=x");
    assert_eq!(stdout, "token=[redacted]");
    assert_eq!(stdout.len(), "token=[redacted]".len());
    assert!(stderr.is_empty());

    // Shrinkage: a long value absorbed into the replacement shrinks the canonical body.
    let (stdout, stderr) = redact_cross_stream_pair("head tok", &format!("en={}", "A".repeat(100)));
    assert_eq!(stdout, "head token=[redacted]");
    assert!(stderr.is_empty());

    // Marker fully inside the stderr token stays attributed to stderr.
    let (stdout, stderr) = redact_cross_stream_pair("head xyz", "token=SECRET tail");
    assert_eq!(stdout, "head xyz");
    assert_eq!(stderr, "token=[redacted] tail");

    // Carry marker at the end of stdout redacts the first stderr token in place.
    let (stdout, stderr) = redact_cross_stream_pair("head --token ", "SUPER-SECRET-123 tail");
    assert_eq!(stdout, "head --token ");
    assert_eq!(stderr, "[redacted] tail");

    // Merged token equal to a carry marker redacts the stderr token AFTER it; the unchanged
    // merged token stays attributed to its source streams.
    let (stdout, stderr) = redact_cross_stream_pair("head --to", "ken SUPER-SECRET-123 tail");
    assert_eq!(stdout, "head --to");
    assert_eq!(stderr, "ken [redacted] tail");
    assert_eq!(
        format!("{stdout}{stderr}"),
        "head --token [redacted] tail",
        "canonical must match the full-text tokenizer output"
    );

    // URL crossing the boundary is projected and attributed to the span-start stream. The
    // per-stream pass already normalized the stdout fragment (trailing slash), so the merged
    // URL keeps the fragmentary host; the sensitive query is still projected away.
    let (stdout, stderr) = redact_cross_stream_pair("head https://exam", "ple.com?token=x tail");
    assert_eq!(stdout, "head https://exam/ple.com?[redacted]");
    assert_eq!(stderr, " tail");
    // The scheme marker itself split at the boundary is reassembled and projected.
    let (stdout, stderr) = redact_cross_stream_pair("head https:/", "/exa.com?token=x tail");
    assert_eq!(stdout, "head https://exa.com/?[redacted]");
    assert_eq!(stderr, " tail");
}

#[test]
fn cross_stream_boundary_leaves_non_merged_and_clean_streams_unchanged() {
    // Whitespace-separated streams are already closed by the per-stream passes.
    let cases = [
        ("line one\n", "\nerror line\n"),
        ("", ""),
        ("no secrets here", "and none here"),
        ("ends with space ", " starts with space"),
        ("--token", ""),
        ("", "token=SECRET"),
    ];
    for (stdout, stderr) in cases {
        let (safe_stdout, safe_stderr) = redact_cross_stream_pair(stdout, stderr);
        assert_eq!(
            safe_stdout,
            safe_persistence_text(stdout),
            "stdout {stdout:?}"
        );
        assert_eq!(
            safe_stderr,
            safe_persistence_text(stderr),
            "stderr {stderr:?}"
        );
    }
}

#[test]
fn cross_stream_boundary_absorbs_carry_value_fragments_like_the_full_text_scan() {
    // The per-stream pass consumed `redact_next` on the last stdout token; the value continues
    // into the first stderr token and must be absorbed into the same `[redacted]` replacement,
    // exactly like the full-text tokenizer over the raw combined bytes.
    let (stdout, stderr) = redact_cross_stream_pair("head --token SUPER-SECRE", "T-123 tail");
    assert_eq!(stdout, "head --token [redacted]");
    assert_eq!(stderr, " tail");
    assert_cross_stream_closed(
        "head --token SUPER-SECRE",
        "T-123 tail",
        &["--token SUPER-SECRET-123"],
    );

    // The literal `[redacted]` tail preceded by a carry marker is indistinguishable from a
    // consumed flag, and the full-text scan absorbs its continuation the same way.
    let (stdout, stderr) = redact_cross_stream_pair("head --token [redacted]", "hello tail");
    assert_eq!(stdout, "head --token [redacted]");
    assert_eq!(stderr, " tail");

    // A value-marker replacement keeps its marker prefix, so the continuation is already
    // absorbed by the merged-token scan; no separate rule is needed.
    let (stdout, stderr) = redact_cross_stream_pair("head token=SUPER-SECRE", "T-123 tail");
    assert_eq!(stdout, "head token=[redacted]");
    assert_eq!(stderr, " tail");

    // A bare `[redacted]` that is NOT preceded by a carry marker must not absorb stderr content.
    let (stdout, stderr) = redact_cross_stream_pair("head token=x [redacted]", "hello tail");
    assert_eq!(stdout, "head token=[redacted] [redacted]");
    assert_eq!(stderr, "hello tail");

    // Whitespace-separated value fragments are kept by the full-text scan too.
    let (stdout, stderr) = redact_cross_stream_pair("head --token SUPER-SECRE ", "T-123 tail");
    assert_eq!(stdout, "head --token [redacted] ");
    assert_eq!(stderr, "T-123 tail");
}

#[test]
fn cross_stream_boundary_is_deterministic() {
    let stdout = "build step 12 of 99 tok";
    let stderr = "en=SUPER-SECRET-123 warning https://exa";
    let first = redact_cross_stream_pair(stdout, stderr);
    for _ in 0..8 {
        assert_eq!(redact_cross_stream_pair(stdout, stderr), first);
    }
    assert_eq!(
        format!("{}{}", first.0, first.1),
        "build step 12 of 99 token=[redacted] warning https://exa/"
    );
}
