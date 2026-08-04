use std::{io::Write, time::Instant};

use anyhow::{Context, Result};

use super::*;
use crate::{ControlEntry, Session, SessionLogEntry, ToolErrorKind};

fn store_fixture() -> Result<(tempfile::TempDir, ToolArtifactStore)> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    Ok((temp, ToolArtifactStore::for_session_store(&session_store)))
}

#[test]
fn opaque_ref_and_blob_path_do_not_expose_each_other() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor = store.capture_text(
        "call-1",
        "shell",
        "hello",
        ToolArtifactSensitivity::Ordinary,
    )?;

    assert!(descriptor.artifact_ref.artifact_id.starts_with("ta1_"));
    assert!(
        !store
            .root()
            .to_string_lossy()
            .contains(&descriptor.artifact_ref.artifact_id)
    );
    assert_eq!(store.read_all(&descriptor)?, b"hello");
    Ok(())
}

#[test]
fn model_preview_limits_never_exceed_the_requested_bytes() {
    let value = "abcdef".repeat(100);
    for limit in [0, 1, 8, 32, 128] {
        let (preview, kind) = bounded_model_preview(&value, limit);
        assert!(preview.len() <= limit);
        assert_eq!(kind, ToolPreviewKind::HeadTail);
    }
}

#[test]
fn high_volume_tools_use_a_smaller_initial_model_view() -> Result<()> {
    let body = "x".repeat(TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    for tool_name in [
        "read_file",
        "mcp__workspace__read_file",
        "mcp__docs__search",
    ] {
        let (recorded, _) = ToolResultRecordedV3::capture(
            &ToolResult::ok(
                format!("call-{tool_name}"),
                tool_name,
                &body,
                crate::ToolResultMeta::default(),
            ),
            None,
            ToolArtifactSensitivity::Ordinary,
        )?;
        assert_eq!(
            recorded.initial_model_view.preview.len(),
            TOOL_MODEL_VIEW_HIGH_VOLUME_MAX_BYTES
        );
    }
    let (shell, _) = ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-shell",
            "shell",
            &body,
            crate::ToolResultMeta::default(),
        ),
        None,
        ToolArtifactSensitivity::Ordinary,
    )?;

    assert_eq!(
        shell.initial_model_view.preview.len(),
        TOOL_MODEL_VIEW_INITIAL_MAX_BYTES
    );
    Ok(())
}

#[test]
fn assistant_batch_preview_budget_is_per_batch_and_not_cumulative_across_batches() -> Result<()> {
    // RFC-0062 11.2/11.3: the 64 KiB preview budget belongs to one assistant tool-call batch.
    // A later batch always receives a fresh budget; history is governed by token aging, never by
    // a root-run byte bucket that hides new current results.
    let mut preview_bytes = 0;
    for index in 0..2 {
        let order = (0..4)
            .map(|i| format!("call-{index}-{i}"))
            .collect::<Vec<_>>();
        let candidates = order
            .iter()
            .map(|id| (id.clone(), 32 * 1024))
            .collect::<std::collections::BTreeMap<_, _>>();
        let limits = allocate_batch_preview_limits(&order, &candidates);
        preview_bytes += limits.values().copied().sum::<usize>();
        assert_eq!(
            limits.values().copied().sum::<usize>(),
            TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES
        );
    }
    assert_eq!(preview_bytes, TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES * 2);
    Ok(())
}

#[test]
fn batch_allocator_guarantees_minimum_preview_floor_for_safe_results() {
    // RFC-0062 11.2: a 128-result oversized batch still gives every safe non-empty result its
    // deterministic 512 B floor, and the total never exceeds the batch budget.
    let order = (0..128).map(|i| format!("call-{i}")).collect::<Vec<_>>();
    let candidates = order
        .iter()
        .map(|id| (id.clone(), 16 * 1024))
        .collect::<std::collections::BTreeMap<_, _>>();
    let limits = allocate_batch_preview_limits(&order, &candidates);
    assert_eq!(limits.len(), 128);
    for limit in limits.values() {
        assert_eq!(*limit, TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES);
    }
    assert_eq!(
        limits.values().copied().sum::<usize>(),
        TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES * 128,
        "128 floors fit exactly inside the 64 KiB batch budget"
    );
}

#[test]
fn batch_allocator_charges_actual_candidate_bytes_and_respects_declaration_order() -> Result<()> {
    // RFC-0062 11.2: allocation follows assistant declaration order; smaller candidates consume
    // only their actual bytes, so later results keep visible previews instead of being hidden by
    // a pre-charged maximum.
    let order = vec![
        "call-a".to_owned(),
        "call-b".to_owned(),
        "call-c".to_owned(),
    ];
    let candidates = std::collections::BTreeMap::from([
        ("call-a".to_owned(), 32 * 1024),
        ("call-b".to_owned(), 1024),
        ("call-c".to_owned(), 32 * 1024),
    ]);
    let limits = allocate_batch_preview_limits(&order, &candidates);
    assert_eq!(limits.get("call-a"), Some(&(512 + 32 * 1024 - 512)));
    assert_eq!(limits.get("call-b"), Some(&1024));
    assert!(limits.get("call-c").copied().unwrap_or(0) > 0);
    let total: usize = limits.values().copied().sum();
    assert!(total <= TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES);
    Ok(())
}

#[test]
fn streaming_capture_is_bounded_and_records_truthful_head_tail() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let half = TOOL_ARTIFACT_MAX_BYTES / 2;
    let mut sink = store.begin_policy_safe_capture(
        "call-large",
        "shell",
        "application/octet-stream",
        ToolArtifactEncoding::Binary,
        ToolArtifactSensitivity::Ordinary,
    );
    sink.write_all(&vec![b'h'; half + 13])?;
    sink.write_all(&vec![b't'; half + 29])?;
    let descriptor = sink.finish()?;

    assert_eq!(
        descriptor.observed_bytes,
        (TOOL_ARTIFACT_MAX_BYTES + 42) as u64
    );
    assert_eq!(descriptor.policy_projected_bytes, descriptor.observed_bytes);
    assert_eq!(descriptor.persisted_bytes, TOOL_ARTIFACT_MAX_BYTES as u64);
    let ToolArtifactCompleteness::StorageTruncated(truncation) = &descriptor.completeness else {
        panic!("large streaming artifact must be storage-truncated");
    };
    assert_eq!(truncation.omitted_bytes, 42);
    assert_eq!(truncation.retained_head_bytes, half as u64);
    assert_eq!(truncation.retained_tail_bytes, half as u64);
    let body = store.read_all(&descriptor)?;
    assert!(body[..half].iter().all(|byte| *byte == b'h'));
    assert!(body[half..].iter().all(|byte| *byte == b't'));
    Ok(())
}

#[test]
fn hundred_megabyte_stream_keeps_only_the_bounded_head_and_tail() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let half = TOOL_ARTIFACT_MAX_BYTES / 2;
    let mut sink = store.begin_policy_safe_capture(
        "call-100m",
        "shell",
        "application/octet-stream",
        ToolArtifactEncoding::Binary,
        ToolArtifactSensitivity::Ordinary,
    );
    sink.write_all(&vec![b'h'; half])?;
    let middle_chunk = vec![b'm'; 64 * 1024];
    let middle_bytes = 100 * 1024 * 1024 - TOOL_ARTIFACT_MAX_BYTES;
    for _ in 0..middle_bytes / middle_chunk.len() {
        sink.write_all(&middle_chunk)?;
    }
    sink.write_all(&vec![b't'; half])?;
    let descriptor = sink.finish()?;

    assert_eq!(descriptor.observed_bytes, 100 * 1024 * 1024);
    assert_eq!(descriptor.persisted_bytes, TOOL_ARTIFACT_MAX_BYTES as u64);
    let ToolArtifactCompleteness::StorageTruncated(truncation) = &descriptor.completeness else {
        panic!("100 MiB stream must be storage-truncated");
    };
    assert_eq!(
        truncation.omitted_bytes,
        (100 * 1024 * 1024 - TOOL_ARTIFACT_MAX_BYTES) as u64
    );
    let body = store.read_all(&descriptor)?;
    assert!(body[..half].iter().all(|byte| *byte == b'h'));
    assert!(body[half..].iter().all(|byte| *byte == b't'));
    Ok(())
}

#[test]
#[ignore = "scale acceptance: streams 1 GiB and persists up to the 256 MiB session budget"]
fn hundred_ten_megabyte_outputs_keep_events_bounded_and_enforce_session_budget() -> Result<()> {
    const OUTPUT_COUNT: usize = 100;
    const OUTPUT_BYTES: usize = 10 * 1024 * 1024;
    const CHUNK_BYTES: usize = 64 * 1024;

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-scale.jsonl");
    let durable_store = JsonlSessionStore::new(&session_path)?;
    let artifact_store = ToolArtifactStore::for_session_store(&durable_store);
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;
    let chunk = vec![b'x'; CHUNK_BYTES];

    for index in 0..OUTPUT_COUNT {
        let call_id = format!("call-scale-{index}");
        let mut sink = artifact_store.begin_policy_safe_capture(
            &call_id,
            "shell",
            "application/octet-stream",
            ToolArtifactEncoding::Binary,
            ToolArtifactSensitivity::Ordinary,
        );
        sink.write_all(&[index as u8])?;
        let remaining = OUTPUT_BYTES - 1;
        for _ in 0..remaining / chunk.len() {
            sink.write_all(&chunk)?;
        }
        sink.write_all(&chunk[..remaining % chunk.len()])?;

        match sink.finish() {
            Ok(descriptor) => {
                accepted += 1;
                assert_eq!(descriptor.observed_bytes, OUTPUT_BYTES as u64);
                assert_eq!(descriptor.persisted_bytes, OUTPUT_BYTES as u64);
                let (recorded, _) = ToolResultRecordedV3::capture(
                    &ToolResult::ok(
                        &call_id,
                        "shell",
                        "bounded scale projection",
                        crate::ToolResultMeta::default(),
                    )
                    .with_captured_artifact(descriptor),
                    Some(&artifact_store),
                    ToolArtifactSensitivity::Ordinary,
                )?;
                let encoded = serde_json::to_vec(&recorded)?;
                assert!(encoded.len() <= TOOL_RESULT_EVENT_TARGET_BYTES);
                durable_store.append(&SessionLogEntry::ToolResultV3(recorded))?;
            }
            Err(error) => {
                rejected += 1;
                assert!(
                    format!("{error:#}").contains("session budget exceeded"),
                    "unexpected scale rejection: {error:#}"
                );
            }
        }
    }

    assert_eq!(accepted, 25);
    assert_eq!(rejected, 75);
    assert!(std::fs::metadata(&session_path)?.len() < 2 * 1024 * 1024);
    Ok(())
}

#[test]
#[ignore = "scale acceptance: fsyncs 1000 immutable artifact/result pairs"]
fn thousand_small_results_keep_incremental_artifact_overhead_bounded() -> Result<()> {
    const OUTPUT_COUNT: usize = 1_000;

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-small-scale.jsonl");
    let durable_store = JsonlSessionStore::new(&session_path)?;
    let artifact_store = ToolArtifactStore::for_session_store(&durable_store);
    let started = Instant::now();
    let mut artifact_bytes = 0_u64;

    for index in 0..OUTPUT_COUNT {
        let call_id = format!("call-small-{index}");
        let body = format!("small-result-{index}");
        artifact_bytes = artifact_bytes.saturating_add(body.len() as u64);
        let (recorded, _) = ToolResultRecordedV3::capture(
            &ToolResult::ok(&call_id, "shell", body, crate::ToolResultMeta::default()),
            Some(&artifact_store),
            ToolArtifactSensitivity::Ordinary,
        )?;
        assert!(serde_json::to_vec(&recorded)?.len() <= TOOL_RESULT_EVENT_TARGET_BYTES);
        durable_store.append(&SessionLogEntry::ToolResultV3(recorded))?;
    }

    let elapsed = started.elapsed();
    let ledger: ToolArtifactBlobUsageLedgerV1 =
        serde_json::from_slice(&std::fs::read(artifact_store.root().join("usage.json"))?)?;
    assert_eq!(ledger.bytes, artifact_bytes);
    assert!(!ledger.dirty);
    assert!(std::fs::metadata(&session_path)?.len() < 64 * 1024 * 1024);
    eprintln!(
        "RFC-0059 small-result scale: {OUTPUT_COUNT} results in {elapsed:?} ({:.3} ms/result), jsonl={} bytes",
        elapsed.as_secs_f64() * 1_000.0 / OUTPUT_COUNT as f64,
        std::fs::metadata(&session_path)?.len(),
    );
    Ok(())
}

#[test]
fn redaction_and_storage_truncation_are_both_preserved() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let policy_bytes = vec![b'x'; TOOL_ARTIFACT_MAX_BYTES + 1];
    let descriptor = store.capture_policy_safe_bytes(
        "call-redacted",
        "shell",
        &policy_bytes,
        policy_bytes.len() as u64 + 20,
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::SensitiveLocal,
        2,
    )?;

    let ToolArtifactCompleteness::PolicyRedacted {
        redaction_count,
        storage_truncation: Some(truncation),
    } = descriptor.completeness
    else {
        panic!("redacted large output must preserve both loss causes");
    };
    assert_eq!(redaction_count, 2);
    assert_eq!(truncation.omitted_bytes, 1);
    assert_eq!(
        descriptor.policy_projected_bytes,
        (TOOL_ARTIFACT_MAX_BYTES + 1) as u64
    );
    assert_eq!(
        descriptor.observed_bytes,
        (TOOL_ARTIFACT_MAX_BYTES + 21) as u64
    );
    Ok(())
}

#[test]
fn cross_session_descriptor_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first = JsonlSessionStore::new(temp.path().join("first.jsonl"))?;
    let second = JsonlSessionStore::new(temp.path().join("second.jsonl"))?;
    let first_store = ToolArtifactStore::for_session_store(&first);
    let second_store = ToolArtifactStore::for_session_store(&second);
    let descriptor = first_store.capture_text(
        "call-1",
        "shell",
        "private",
        ToolArtifactSensitivity::SensitiveLocal,
    )?;

    assert!(second_store.read_all(&descriptor).is_err());
    assert_eq!(
        second_store.availability(&descriptor),
        ToolArtifactAvailability::PolicyRevoked
    );
    Ok(())
}

#[test]
fn invalid_pre_captured_descriptor_fails_closed_without_recapturing_inline_preview() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_store = ToolArtifactStore::for_session_path(&temp.path().join("source.jsonl"));
    let target_store = ToolArtifactStore::for_session_path(&temp.path().join("target.jsonl"));
    let descriptor = source_store.capture_text(
        "call-1",
        "shell",
        "source artifact body",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let result = ToolResult::ok(
        "call-1",
        "shell",
        "inline preview must not be recaptured",
        crate::ToolResultMeta::default(),
    )
    .with_captured_artifact(descriptor);

    let (recorded, _) = ToolResultRecordedV3::capture(
        &result,
        Some(&target_store),
        ToolArtifactSensitivity::Ordinary,
    )?;

    let ToolArtifactBindingV1::Unavailable { unavailable } = recorded.artifact else {
        panic!("cross-session pre-capture must fail closed");
    };
    assert_eq!(
        unavailable.availability,
        ToolArtifactAvailability::PolicyRevoked
    );
    assert_eq!(
        recorded.capture_telemetry.capture_path,
        ToolResultCapturePathV1::PreCapturedArtifact
    );
    assert!(target_store.manifest_inventory()?.is_empty());
    Ok(())
}

#[test]
fn oversized_inline_capture_hits_hard_guard_without_publishing_raw_body() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let body = format!(
        "head:{}:guarded-secret-sentinel:{}:tail",
        "h".repeat(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES),
        "t".repeat(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES)
    );
    let result = ToolResult::ok(
        "legacy-large",
        "legacy_tool",
        body,
        crate::ToolResultMeta::default(),
    );

    let (recorded, display) = ToolResultRecordedV3::capture(
        &result,
        Some(&store),
        ToolArtifactSensitivity::SensitiveLocal,
    )?;

    let ToolArtifactBindingV1::Unavailable { unavailable } = &recorded.artifact else {
        panic!("oversized inline body must not be published");
    };
    assert_eq!(
        unavailable.availability,
        ToolArtifactAvailability::Unavailable
    );
    assert_eq!(
        recorded.capture_telemetry,
        ToolResultCaptureTelemetryV1 {
            capture_path: ToolResultCapturePathV1::InlineCapture,
            observed_inline_bytes: result.content.len() as u64,
            hard_guard_bytes: Some(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64),
            hard_guard_exceeded: true,
            inline_projection_truncated: true,
        }
    );
    assert_eq!(
        recorded.initial_model_view.preview_kind,
        ToolPreviewKind::HeadTail
    );
    assert!(
        !recorded
            .model_content()?
            .contains("guarded-secret-sentinel")
    );
    assert!(!display.preview.contains("guarded-secret-sentinel"));
    assert!(store.manifest_inventory()?.is_empty());
    assert!(serde_json::to_vec(&recorded)?.len() < TOOL_RESULT_EVENT_TARGET_BYTES);
    Ok(())
}

#[test]
fn bounded_inline_capture_remains_available_and_reports_telemetry() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let result = ToolResult::ok(
        "legacy-small",
        "legacy_tool",
        "small legacy body",
        crate::ToolResultMeta::default(),
    );

    let (recorded, _) =
        ToolResultRecordedV3::capture(&result, Some(&store), ToolArtifactSensitivity::Ordinary)?;

    assert!(matches!(
        recorded.artifact,
        ToolArtifactBindingV1::Published { .. }
    ));
    assert_eq!(
        recorded.capture_telemetry,
        ToolResultCaptureTelemetryV1 {
            capture_path: ToolResultCapturePathV1::InlineCapture,
            observed_inline_bytes: result.content.len() as u64,
            hard_guard_bytes: Some(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64),
            hard_guard_exceeded: false,
            inline_projection_truncated: false,
        }
    );
    Ok(())
}

#[test]
fn forked_descriptor_gets_independent_ref_and_destination_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_store = ToolArtifactStore::for_session_path(&temp.path().join("source.jsonl"));
    let destination_store =
        ToolArtifactStore::for_session_path(&temp.path().join("destination.jsonl"));
    let source = source_store.capture_text(
        "call-fork",
        "shell",
        "fork evidence",
        ToolArtifactSensitivity::Ordinary,
    )?;

    let destination = destination_store.fork_descriptor_from(&source_store, &source)?;

    assert_ne!(destination.artifact_ref, source.artifact_ref);
    assert_ne!(
        destination.session_scope_id_hash,
        source.session_scope_id_hash
    );
    assert_eq!(destination.content_sha256, source.content_sha256);
    assert_eq!(destination.completeness, source.completeness);
    assert_eq!(destination_store.read_all(&destination)?, b"fork evidence");
    assert!(destination_store.resolve(&source.artifact_ref).is_err());
    assert!(source_store.resolve(&destination.artifact_ref).is_err());
    Ok(())
}

#[test]
fn manifest_gc_retains_active_hold_and_tombstones_only_grace_expired_orphan() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let active = store.capture_text(
        "call-active",
        "shell",
        "active",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let held = store.capture_text(
        "call-held",
        "shell",
        "held",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let orphan = store.capture_text(
        "call-orphan",
        "shell",
        "orphan",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let newest_manifest_ms = store
        .manifest_inventory()?
        .iter()
        .map(|entry| entry.manifest_modified_at_unix_ms)
        .max()
        .unwrap_or(0);
    let roots = ToolArtifactGcRootsV1 {
        active_result_refs: BTreeSet::from([active.artifact_ref.clone()]),
        explicit_holds: BTreeSet::from([held.artifact_ref.clone()]),
        ..ToolArtifactGcRootsV1::default()
    };

    let report = store.garbage_collect(
        &roots,
        newest_manifest_ms.saturating_add(TOOL_ARTIFACT_ORPHAN_GRACE_MS),
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;

    assert_eq!(report.scanned_manifests, 3);
    assert_eq!(report.retained_manifests, 2);
    assert_eq!(report.tombstoned_manifests, 1);
    assert_eq!(report.tombstoned_blobs, 1);
    assert_eq!(store.read_all(&active)?, b"active");
    assert_eq!(store.read_all(&held)?, b"held");
    assert!(store.resolve(&orphan.artifact_ref).is_err());
    assert!(
        store
            .root()
            .join("trash")
            .join(report.tombstone_id)
            .join("refs")
            .join(format!("{}.json", orphan.artifact_ref.artifact_id))
            .is_file()
    );
    let early = store.prune_garbage_trash(current_unix_ms(), TOOL_ARTIFACT_ORPHAN_GRACE_MS)?;
    assert_eq!(early.removed_tombstones, 0);
    let pruned = store.prune_garbage_trash(u64::MAX, TOOL_ARTIFACT_ORPHAN_GRACE_MS)?;
    assert_eq!(pruned.removed_tombstones, 1);
    assert!(pruned.removed_bytes > 0);
    Ok(())
}

#[test]
fn manifest_gc_initializes_an_absent_session_artifact_namespace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = ToolArtifactStore::for_session_path(&temp.path().join("legacy-session.jsonl"));
    assert!(!store.root().exists());

    let report = store.garbage_collect(
        &ToolArtifactGcRootsV1::default(),
        current_unix_ms(),
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;

    assert_eq!(report.scanned_manifests, 0);
    assert_eq!(report.tombstoned_manifests, 0);
    assert!(store.root().is_dir());
    assert!(store.root().join("refs").is_dir());
    assert!(store.root().join("trash").is_dir());
    Ok(())
}

#[test]
fn manifest_gc_skips_artifact_with_concurrent_read_lease() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let artifact = store.capture_text(
        "call-read-race",
        "shell",
        "concurrent read",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let modified = store.manifest_inventory()?[0].manifest_modified_at_unix_ms;
    let read_lock = store.open_ref_lock(&artifact.artifact_ref)?;
    read_lock.try_lock_shared()?;

    let skipped = store.garbage_collect(
        &ToolArtifactGcRootsV1::default(),
        modified.saturating_add(TOOL_ARTIFACT_ORPHAN_GRACE_MS),
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;

    assert_eq!(skipped.skipped_active_reads, 1);
    assert_eq!(skipped.tombstoned_manifests, 0);
    assert_eq!(store.resolve(&artifact.artifact_ref)?, artifact);
    drop(read_lock);
    let collected = store.garbage_collect(
        &ToolArtifactGcRootsV1::default(),
        modified.saturating_add(TOOL_ARTIFACT_ORPHAN_GRACE_MS),
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;
    assert_eq!(collected.tombstoned_manifests, 1);
    Ok(())
}

#[test]
fn manifest_gc_recovers_grace_expired_staging_and_published_blob_orphans() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let staging = store.root().join("staging");
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("crash.part"), b"staging orphan")?;

    let orphan_bytes = b"published before descriptor append";
    let orphan_hash = stable_event_hash(orphan_bytes);
    let digest = orphan_hash.strip_prefix("sha256:").context("hash prefix")?;
    let blob_dir = store.root().join("blobs").join(&digest[..2]);
    std::fs::create_dir_all(&blob_dir)?;
    std::fs::write(blob_dir.join(format!("{digest}.blob")), orphan_bytes)?;

    let report = store.garbage_collect(
        &ToolArtifactGcRootsV1::default(),
        u64::MAX,
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;

    assert_eq!(report.scanned_manifests, 0);
    assert_eq!(report.tombstoned_manifests, 0);
    assert_eq!(report.tombstoned_blobs, 1);
    assert_eq!(report.tombstoned_orphan_blobs, 1);
    assert_eq!(report.tombstoned_staging_files, 1);
    assert!(report.tombstoned_bytes >= orphan_bytes.len() as u64);
    assert!(std::fs::read_dir(staging)?.next().is_none());
    assert!(std::fs::read_dir(blob_dir)?.next().is_none());
    Ok(())
}

#[test]
fn dirty_blob_usage_ledger_is_reconciled_before_the_next_publish() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    store.capture_text(
        "call-ledger-a",
        "shell",
        "alpha",
        ToolArtifactSensitivity::Ordinary,
    )?;
    store.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
        schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
        bytes: 0,
        dirty: true,
    })?;

    store.capture_text(
        "call-ledger-b",
        "shell",
        "beta",
        ToolArtifactSensitivity::Ordinary,
    )?;

    let ledger: ToolArtifactBlobUsageLedgerV1 =
        serde_json::from_slice(&std::fs::read(store.root().join("usage.json"))?)?;
    assert_eq!(
        ledger,
        ToolArtifactBlobUsageLedgerV1 {
            schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
            bytes: 9,
            dirty: false,
        }
    );
    assert_eq!(directory_file_bytes(&store.root().join("blobs"))?, 9);
    Ok(())
}

#[test]
fn corrupt_blob_is_detected_without_returning_body() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor = store.capture_text(
        "call-1",
        "shell",
        "evidence",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let digest = descriptor
        .content_sha256
        .strip_prefix("sha256:")
        .expect("hash prefix");
    let blob = store
        .root()
        .join("blobs")
        .join(&digest[..2])
        .join(format!("{digest}.blob"));
    std::fs::write(blob, b"tampered")?;

    assert_eq!(
        store.availability(&descriptor),
        ToolArtifactAvailability::HashMismatch
    );
    assert!(store.read_all(&descriptor).is_err());
    Ok(())
}

#[test]
fn durable_v2_event_is_bounded_while_artifact_keeps_the_complete_body() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::new("test", "model").with_store(store);
    let body = format!(
        "head:{}:middle-sentinel:{}:tail",
        "0123456789".repeat(100_000),
        "abcdefghij".repeat(100_000)
    );
    let artifact_store = session.tool_artifact_store().expect("durable store");
    let pre_captured = artifact_store.capture_text(
        "call-large",
        "shell",
        &body,
        ToolArtifactSensitivity::Ordinary,
    )?;
    let result = ToolResult::ok(
        "call-large",
        "shell",
        "bounded shell projection",
        crate::ToolResultMeta::default(),
    )
    .with_captured_artifact(pre_captured);
    let (recorded, display) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let descriptor = recorded
        .artifact
        .descriptor()
        .expect("artifact must be published")
        .clone();
    assert_eq!(
        recorded.capture_telemetry.capture_path,
        ToolResultCapturePathV1::PreCapturedArtifact
    );

    session.append_tool_result_bundle(recorded, Vec::new())?;

    let jsonl = std::fs::read(&session_path)?;
    assert!(jsonl.len() < TOOL_RESULT_EVENT_TARGET_BYTES);
    assert!(
        !jsonl
            .windows(b"middle-sentinel".len())
            .any(|window| window == b"middle-sentinel")
    );
    assert_eq!(artifact_store.read_all(&descriptor)?, body.as_bytes());
    assert!(display.preview.len() <= TOOL_DISPLAY_VIEW_MAX_BYTES);
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    assert_eq!(
        records[0].stored_event().event_kind(),
        Some(crate::DurableEventType::ToolResultRecordedV3)
    );
    assert!(matches!(
        records[0].session_log_entry()?,
        Some(SessionLogEntry::ToolResultV3(_))
    ));
    Ok(())
}

#[test]
fn typed_retrieval_resolves_opaque_ref_without_jsonl_scan() -> Result<()> {
    let (temp, store) = store_fixture()?;
    let content = "zero\nalpha target\nbeta\nalpha target again\nomega\n";
    let descriptor = store.capture_text(
        "call-read",
        "shell",
        content,
        ToolArtifactSensitivity::Ordinary,
    )?;
    let session_path = temp.path().join("session.jsonl");
    if session_path.exists() {
        std::fs::remove_file(session_path)?;
    }

    let byte_page = store.read_page(
        &descriptor.artifact_ref,
        ToolArtifactSelectorV1::ByteSlice {
            offset: 5,
            limit: 7,
        },
    )?;
    assert_eq!(byte_page.body_encoding, ToolArtifactPageEncoding::Utf8);
    assert_eq!(byte_page.body, "alpha t");
    assert_eq!(byte_page.returned_bytes, 7);

    let line_page = store.read_page(
        &descriptor.artifact_ref,
        ToolArtifactSelectorV1::LinePage {
            start_line: 1,
            line_count: 2,
        },
    )?;
    assert_eq!(line_page.body, "alpha target\nbeta\n");
    assert!(!line_page.eof);

    let search_page = store.read_page(
        &descriptor.artifact_ref,
        ToolArtifactSelectorV1::SearchLiteral {
            query: "target".to_owned(),
            start_offset: 0,
            max_matches: 2,
            context_lines: 1,
        },
    )?;
    assert_eq!(search_page.match_count, 2);
    assert!(search_page.body.contains("alpha target"));
    assert!(search_page.body.contains("alpha target again"));
    assert!(search_page.returned_bytes <= TOOL_ARTIFACT_READ_MAX_BYTES);

    let overlapping = store.capture_text(
        "call-overlapping-search",
        "shell",
        "aaa\n",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let overlapping_page = store.read_page(
        &overlapping.artifact_ref,
        ToolArtifactSelectorV1::SearchLiteral {
            query: "aa".to_owned(),
            start_offset: 0,
            max_matches: 2,
            context_lines: 0,
        },
    )?;
    assert_eq!(overlapping_page.match_count, 2);
    Ok(())
}

#[test]
fn literal_search_is_linear_on_repetitive_max_artifact() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let content = vec![b'a'; TOOL_ARTIFACT_MAX_BYTES];
    let descriptor = store.capture_policy_safe_bytes(
        "call-adversarial-search",
        "shell",
        &content,
        content.len() as u64,
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::Ordinary,
        0,
    )?;
    // Every candidate shares a 511-byte prefix with the query. A windows-based matcher performs
    // O(artifact_bytes * query_bytes) comparisons; the compiled matcher stays linear.
    let query = format!("{}b", "a".repeat(511));
    let page = store.read_page(
        &descriptor.artifact_ref,
        ToolArtifactSelectorV1::SearchLiteral {
            query,
            start_offset: 0,
            max_matches: 1,
            context_lines: 0,
        },
    )?;

    assert_eq!(page.match_count, 0);
    assert!(page.body.is_empty());
    assert!(page.eof);
    Ok(())
}

#[test]
fn selectors_reject_unbounded_requests() {
    assert!(
        ToolArtifactSelectorV1::ByteSlice {
            offset: 0,
            limit: TOOL_ARTIFACT_READ_MAX_BYTES + 1,
        }
        .validate()
        .is_err()
    );
    assert!(
        ToolArtifactSelectorV1::LinePage {
            start_line: 0,
            line_count: TOOL_ARTIFACT_READ_MAX_LINES + 1,
        }
        .validate()
        .is_err()
    );
    assert!(
        ToolArtifactSelectorV1::SearchLiteral {
            query: "x".to_owned(),
            start_offset: 0,
            max_matches: TOOL_ARTIFACT_SEARCH_MAX_MATCHES + 1,
            context_lines: 0,
        }
        .validate()
        .is_err()
    );
}

#[test]
fn per_turn_budget_is_shared_and_counts_attempts() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor = store.capture_text(
        "call-budget",
        "shell",
        "0123456789",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let budget = ToolArtifactReadBudgetV1::default();
    let sibling = budget.clone();
    for offset in 0..TOOL_ARTIFACT_READS_PER_TURN {
        sibling.read_page(
            &store,
            &descriptor.artifact_ref,
            ToolArtifactSelectorV1::ByteSlice {
                offset: offset as u64,
                limit: 1,
            },
        )?;
    }
    assert!(
        budget
            .read_page(
                &store,
                &descriptor.artifact_ref,
                ToolArtifactSelectorV1::ByteSlice {
                    offset: 0,
                    limit: 1,
                },
            )
            .is_err()
    );
    assert_eq!(budget.usage(), (TOOL_ARTIFACT_READS_PER_TURN, 8));
    Ok(())
}

#[test]
fn read_receipt_event_is_body_free_and_recovery_critical() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::new("test", "model").with_store(store);
    let receipt = ToolArtifactReadRecordedV1 {
        schema_version: TOOL_ARTIFACT_READ_SCHEMA_VERSION,
        call_id: "read-call".to_owned(),
        artifact_ref: ToolArtifactRefV1::random(),
        source_descriptor_event_id: "source-event".to_owned(),
        active_epoch_id: "context-epoch:test".to_owned(),
        selector: ToolArtifactSelectorV1::ByteSlice {
            offset: 0,
            limit: 10,
        },
        returned_bytes: 10,
        page_sha256: stable_event_hash(b"page bytes"),
        artifact_sha256: stable_event_hash(b"artifact bytes"),
        outcome: ToolArtifactReadOutcome::Returned,
        deduplicated_from_call_id: None,
    };
    session.append_control(ControlEntry::ToolArtifactRead(receipt.clone()))?;

    let jsonl = std::fs::read_to_string(&session_path)?;
    assert!(!jsonl.contains("page bytes"));
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    assert_eq!(
        records[0].stored_event().event_kind(),
        Some(crate::DurableEventType::ToolArtifactReadRecorded)
    );
    assert!(matches!(
        records[0].session_log_entry()?,
        Some(SessionLogEntry::Control(ControlEntry::ToolArtifactRead(value)))
            if value == receipt
    ));
    Ok(())
}

#[test]
fn utf8_preview_cap_boundary_never_splits_code_points() -> Result<()> {
    // "α" is two UTF-8 bytes, so the exact cap is reachable without splitting a code point.
    let unit = "α";
    let unit_bytes = unit.len();
    let body = unit.repeat(TOOL_MODEL_VIEW_INITIAL_MAX_BYTES / unit_bytes);
    let exact = body.len();
    assert_eq!(exact, TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);

    let (preview_exact, kind_exact) =
        bounded_model_preview(&body, TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    assert_eq!(preview_exact.len(), TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    assert_eq!(kind_exact, ToolPreviewKind::Complete);

    let overflow = format!("{body}α");
    let (preview_overflow, kind_overflow) =
        bounded_model_preview(&overflow, TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    assert!(preview_overflow.len() <= TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    assert_eq!(kind_overflow, ToolPreviewKind::HeadTail);
    assert!(preview_overflow.is_char_boundary(preview_overflow.len()));
    assert!(std::str::from_utf8(preview_overflow.as_bytes()).is_ok());

    let (recorded, display) = ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-utf8",
            "shell",
            &overflow,
            crate::ToolResultMeta::default(),
        ),
        None,
        ToolArtifactSensitivity::Ordinary,
    )?;
    let preview = recorded.initial_model_view.preview.clone();
    assert!(preview.len() <= TOOL_MODEL_VIEW_INITIAL_MAX_BYTES);
    assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    assert!(preview.is_char_boundary(preview.len()));
    assert_eq!(
        recorded.initial_model_view_sha256,
        stable_event_hash(recorded.model_content()?.as_bytes())
    );
    assert!(display.preview.is_char_boundary(display.preview.len()));
    Ok(())
}

#[test]
fn page_read_dedupe_is_epoch_scoped() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor = store.capture_text(
        "call-dedupe",
        "shell",
        "first line\nsecond line\nthird line\n",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let selector = ToolArtifactSelectorV1::LinePage {
        start_line: 0,
        line_count: 3,
    };
    let budget = ToolArtifactReadBudgetV1::default();
    let first = budget.read_page_for_call(
        &store,
        &descriptor.artifact_ref,
        selector.clone(),
        "call-a",
        "context-epoch:root",
    )?;
    assert!(first.deduplicated_from_call_id.is_none());
    assert!(first.page.body.contains("first line"));

    let repeat = budget.read_page_for_call(
        &store,
        &descriptor.artifact_ref,
        selector.clone(),
        "call-b",
        "context-epoch:root",
    )?;
    assert_eq!(
        repeat.deduplicated_from_call_id.as_deref(),
        Some("call-a"),
        "same epoch must dedupe the immutable page"
    );

    let after_rotation = budget.read_page_for_call(
        &store,
        &descriptor.artifact_ref,
        selector.clone(),
        "call-c",
        "context-epoch:tool-aging-1",
    )?;
    assert!(
        after_rotation.deduplicated_from_call_id.is_none(),
        "a rotated context epoch must not assume the provider remembers the old page"
    );
    assert!(after_rotation.page.body.contains("first line"));
    assert_eq!(
        budget.usage().0,
        2,
        "the rotated read consumes a fresh per-epoch delivery"
    );
    Ok(())
}

#[test]
fn corrupt_blob_page_read_fails_closed_without_partial_content() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor = store.capture_text(
        "call-corrupt-page",
        "shell",
        "0123456789",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let digest = descriptor
        .content_sha256
        .strip_prefix("sha256:")
        .expect("hash prefix");
    let blob = store
        .root()
        .join("blobs")
        .join(&digest[..2])
        .join(format!("{digest}.blob"));
    std::fs::write(&blob, b"tampered")?;

    let selector = ToolArtifactSelectorV1::ByteSlice {
        offset: 0,
        limit: 4,
    };
    let error = store
        .read_page(&descriptor.artifact_ref, selector.clone())
        .expect_err("corrupt blob must fail closed");
    assert!(
        format!("{error:#}").contains("content hash mismatch"),
        "unexpected error: {error:#}"
    );
    let budget = ToolArtifactReadBudgetV1::default();
    assert!(
        budget
            .read_page_for_call(
                &store,
                &descriptor.artifact_ref,
                selector,
                "call-corrupt",
                "context-epoch:root",
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn grace_expired_artifact_retrieval_fails_closed_as_missing() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let orphan = store.capture_text(
        "call-expired",
        "shell",
        "expired evidence",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let newest_manifest_ms = store
        .manifest_inventory()?
        .iter()
        .map(|entry| entry.manifest_modified_at_unix_ms)
        .max()
        .unwrap_or(0);
    let report = store.garbage_collect(
        &ToolArtifactGcRootsV1::default(),
        newest_manifest_ms.saturating_add(TOOL_ARTIFACT_ORPHAN_GRACE_MS),
        TOOL_ARTIFACT_ORPHAN_GRACE_MS,
    )?;
    assert_eq!(report.tombstoned_manifests, 1);

    assert_eq!(
        store.availability(&orphan),
        ToolArtifactAvailability::Missing
    );
    assert!(store.resolve(&orphan.artifact_ref).is_err());
    assert!(store.read_all(&orphan).is_err());
    assert!(
        store
            .read_page(
                &orphan.artifact_ref,
                ToolArtifactSelectorV1::ByteSlice {
                    offset: 0,
                    limit: 4,
                },
            )
            .is_err(),
        "expired artifact must never yield a page"
    );
    Ok(())
}

#[test]
fn unavailable_artifact_display_never_advertises_has_more() -> Result<()> {
    let result = ToolResult::ok(
        "call-unavailable",
        "shell",
        "x".repeat(TOOL_MODEL_VIEW_INITIAL_MAX_BYTES + 1024),
        crate::ToolResultMeta::default(),
    )
    .with_unavailable_artifact_capture((TOOL_MODEL_VIEW_INITIAL_MAX_BYTES + 4096) as u64);
    let (recorded, display) =
        ToolResultRecordedV3::capture(&result, None, ToolArtifactSensitivity::Ordinary)?;
    assert!(matches!(
        recorded.artifact,
        ToolArtifactBindingV1::Unavailable { .. }
    ));
    assert!(
        display.observed_bytes > display.preview.len() as u64,
        "preview must still be a truncation of observed output"
    );
    assert!(
        !display.has_more,
        "an unavailable artifact must not advertise content that cannot be retrieved"
    );
    assert!(
        !display
            .display_capabilities
            .contains(&ToolDisplayCapability::ReadNextPage)
    );
    let restored = recorded.display_view();
    assert!(!restored.has_more);
    Ok(())
}

#[test]
fn oversized_error_message_is_bounded_in_facts_without_failing_capture() -> Result<()> {
    // RFC-0062 5.2/9.5: a large tool error body must never overflow facts or abort the run;
    // the error summary is capped at 1 KiB with UTF-8-safe truncation while the full body
    // stays on the result content and flows into the artifact.
    let (_temp, store) = store_fixture()?;
    let oversized_message = "error detail ".repeat(1024);
    assert!(oversized_message.len() > 8 * 1024);
    let result = ToolResult::error(
        "call-large-error",
        "shell",
        ToolErrorKind::ExitStatus,
        oversized_message,
    );
    let (recorded, display) =
        ToolResultRecordedV3::capture(&result, Some(&store), ToolArtifactSensitivity::Ordinary)?;
    let error = recorded
        .facts
        .error
        .clone()
        .expect("error facts are preserved");
    assert!(
        error.message.len() <= TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES,
        "error summary must stay bounded"
    );
    assert!(error.message.starts_with("error detail "));
    assert_eq!(recorded.facts.status, "error");
    assert_eq!(display.status_label, "error");
    let encoded = serde_json::to_vec(&recorded)?;
    assert!(encoded.len() <= TOOL_RESULT_EVENT_TARGET_BYTES);
    Ok(())
}

#[test]
fn capture_failure_settles_bounded_terminal_fallback() -> Result<()> {
    // RFC-0062 10.5: when the regular projection fails, the record layer settles a bounded
    // terminal fallback instead of amplifying a tool error into an agent runtime failure.
    let result = ToolResult::error(
        "call-fallback",
        "shell",
        ToolErrorKind::ExitStatus,
        "failing projection",
    );
    let (recorded, display) = ToolResultRecordedV3::terminal_fallback(
        &result,
        ToolArtifactSensitivity::Ordinary,
        &anyhow::anyhow!("injected projection failure"),
    )?;
    assert!(matches!(
        recorded.artifact,
        ToolArtifactBindingV1::Unavailable { .. }
    ));
    assert!(recorded.initial_model_view.preview.is_empty());
    assert_eq!(recorded.facts.status, "error");
    assert!(!display.has_more);
    assert!(recorded.initial_model_view_sha256.starts_with("sha256:"));
    let encoded = serde_json::to_vec(&recorded)?;
    assert!(encoded.len() <= TOOL_RESULT_EVENT_TARGET_BYTES);
    Ok(())
}

#[test]
fn empty_result_publishes_a_complete_zero_byte_artifact() -> Result<()> {
    let (_temp, store) = store_fixture()?;
    let descriptor =
        store.capture_text("call-empty", "shell", "", ToolArtifactSensitivity::Ordinary)?;
    assert_eq!(descriptor.observed_bytes, 0);
    assert_eq!(descriptor.policy_projected_bytes, 0);
    assert_eq!(descriptor.persisted_bytes, 0);
    assert!(matches!(
        descriptor.completeness,
        ToolArtifactCompleteness::Complete
    ));
    assert!(
        store.read_all(&descriptor).is_err(),
        "a zero-byte artifact has nothing to retrieve and must fail closed"
    );
    assert!(
        store
            .read_page(
                &descriptor.artifact_ref,
                ToolArtifactSelectorV1::ByteSlice {
                    offset: 0,
                    limit: 1,
                },
            )
            .is_err(),
        "a zero-byte artifact has nothing to retrieve and must fail closed"
    );
    Ok(())
}

#[test]
fn has_more_compares_against_the_bytes_actually_displayed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let session = Session::new("test", "model").with_store(store);
    let artifact_store = session.tool_artifact_store().expect("durable store");

    // Between the model cap (16 KiB) and the display cap (~31 KiB): a complete 20 KiB inline
    // output is fully shown in the live display, so live has_more must be false even though the
    // model preview was truncated to 16 KiB.
    let body_between = "x".repeat(TOOL_MODEL_VIEW_INITIAL_MAX_BYTES + 4096);
    let result = ToolResult::ok(
        "call-between",
        "shell",
        &body_between,
        crate::ToolResultMeta::default(),
    );
    let (recorded, live) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    assert_eq!(
        recorded.initial_model_view.preview.len(),
        TOOL_MODEL_VIEW_INITIAL_MAX_BYTES,
        "model preview stays truncated to its own cap"
    );
    assert_eq!(
        live.preview.len(),
        body_between.len(),
        "display shows the whole body"
    );
    assert!(
        !live.has_more,
        "a complete output fully shown in the display must not advertise more"
    );
    assert!(
        live.display_capabilities
            .contains(&ToolDisplayCapability::ReadNextPage)
    );

    // Restore has no display preview beyond the durable model view, so the same record shows
    // 16 KiB of 20 KiB and must truthfully report has_more.
    let restored = recorded.display_view();
    assert!(restored.preview.len() <= recorded.initial_model_view.preview.len());
    assert!(
        restored.has_more,
        "restored view shows fewer bytes than persisted, so more is retrievable"
    );

    // Beyond the display cap: the live display is truncated and paging is advertised.
    let body_over = "y".repeat(TOOL_DISPLAY_VIEW_MAX_BYTES + 8192);
    let result = ToolResult::ok(
        "call-over",
        "shell",
        &body_over,
        crate::ToolResultMeta::default(),
    );
    let (recorded, live) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    assert!(live.preview.len() < body_over.len());
    assert!(live.has_more);
    assert!(recorded.display_view().has_more);

    // Small complete output: no more in either live or restored view.
    let body_small = "z".repeat(4096);
    let result = ToolResult::ok(
        "call-small",
        "shell",
        &body_small,
        crate::ToolResultMeta::default(),
    );
    let (recorded, live) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    assert_eq!(live.preview.len(), body_small.len());
    assert!(!live.has_more);
    assert!(!recorded.display_view().has_more);
    Ok(())
}

#[test]
fn v2_tool_result_session_is_rejected_with_bounded_diagnostic() -> Result<()> {
    // RFC-0062 9.7 clean cutover: a session containing a legacy tool_result_recorded_v2 event is
    // rejected wholesale with a bounded schema diagnostic; nothing is migrated, aliased, or
    // partially restored, and no tool can be re-executed against it.
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("legacy-v2.jsonl");
    let v2_payload = serde_json::json!({
        "session_log_entry": {
            "tool_result_recorded_v2": {
                "schema_version": 2,
                "message_id": "legacy-message",
                "call_id": "legacy-call",
                "tool_name": "shell",
                "artifact": { "kind": "unavailable", "unavailable": {
                    "availability": "unavailable",
                    "observed_bytes": 0,
                    "reason": "legacy"
                } },
                "facts": {
                    "status": "ok",
                    "exit_code": null,
                    "duration_ms": null,
                    "changed_files": [],
                    "error": null,
                    "mutation_receipt_refs": [],
                    "approval_receipt_refs": [],
                    "verification_receipt_refs": [],
                    "external_provenance_refs": [],
                    "tool_specific": {}
                },
                "initial_model_view": {
                    "preview": "legacy preview",
                    "preview_kind": "complete",
                    "artifact_ref": null,
                    "retrieval_hint": null,
                    "token_upper_bound": 2,
                    "projection_version": 1
                },
                "initial_model_view_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "capture_telemetry": {
                    "capture_path": "inline_capture",
                    "observed_inline_bytes": 14,
                    "hard_guard_bytes": 262144,
                    "hard_guard_exceeded": false,
                    "inline_projection_truncated": false
                },
                "recorded_at_ms": 1
            }
        }
    });
    let mut record = crate::StoredEvent {
        schema_version: crate::STORED_EVENT_SCHEMA_VERSION,
        event_type: "tool_result_recorded_v2".to_owned(),
        event_version: 1,
        event_class: crate::EventClass::Critical,
        event_id: "legacy-event-1".to_owned(),
        session_id: "legacy-session".to_owned(),
        stream_sequence: 1,
        occurred_at: None,
        correlation_id: None,
        causation_id: None,
        parent_session_id: None,
        record_checksum: String::new(),
        payload: v2_payload,
    };
    record.record_checksum = record.compute_record_checksum()?;
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&record)?),
    )?;

    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let error = records[0]
        .session_log_entry()
        .expect_err("legacy V2 session must be rejected on decode");
    let message = format!("{error:#}");
    assert!(
        message.contains("unsupported session schema"),
        "unexpected diagnostic: {message}"
    );
    assert!(message.contains("tool-result-v3"));
    let file_bytes = std::fs::read(&session_path)?;
    assert!(
        file_bytes
            .windows(b"legacy preview".len())
            .any(|w| w == b"legacy preview"),
        "rejection must never rewrite the original file"
    );
    Ok(())
}

#[test]
fn availability_change_events_are_generation_guarded_and_durable() -> Result<()> {
    // RFC-0062 9.4: availability is append-only control state; transitions are generation
    // ordered and the session log keeps the body-free audit trail.
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::new("test", "model").with_store(store);
    let artifact_ref = ToolArtifactRefV1::random();
    let disable = ToolArtifactAvailabilityChangedV1 {
        schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
        artifact_ref: artifact_ref.clone(),
        expected_generation: 0,
        generation: 1,
        previous: ToolArtifactAvailabilityStateV1::Available,
        next: ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
        reason: ToolArtifactAvailabilityReasonV1::GcDisable,
        changed_at_ms: 1,
    };
    disable.validate()?;
    session.append_control(ControlEntry::ToolArtifactAvailabilityChanged(
        disable.clone(),
    ))?;
    let expired = ToolArtifactAvailabilityChangedV1 {
        schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
        artifact_ref,
        expected_generation: 1,
        generation: 2,
        previous: ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
        next: ToolArtifactAvailabilityStateV1::Expired,
        reason: ToolArtifactAvailabilityReasonV1::GcExpired,
        changed_at_ms: 2,
    };
    expired.validate()?;
    session.append_control(ControlEntry::ToolArtifactAvailabilityChanged(
        expired.clone(),
    ))?;

    let jsonl = std::fs::read_to_string(&session_path)?;
    assert!(!jsonl.contains("tool artifact body"));
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let entries = records
        .iter()
        .map(|record| record.session_log_entry())
        .collect::<Result<Vec<_>>>()?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        Some(SessionLogEntry::Control(ControlEntry::ToolArtifactAvailabilityChanged(value)))
            if *value == disable
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        Some(SessionLogEntry::Control(ControlEntry::ToolArtifactAvailabilityChanged(value)))
            if *value == expired
    )));

    // Terminal states never return to Available and stale generations fail closed.
    let illegal = ToolArtifactAvailabilityChangedV1 {
        schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
        artifact_ref: ToolArtifactRefV1::random(),
        expected_generation: 0,
        generation: 1,
        previous: ToolArtifactAvailabilityStateV1::Expired,
        next: ToolArtifactAvailabilityStateV1::Available,
        reason: ToolArtifactAvailabilityReasonV1::PolicyRevoked,
        changed_at_ms: 3,
    };
    assert!(illegal.validate().is_err());
    Ok(())
}

#[test]
fn process_capture_canonical_hash_is_identical_across_chunk_schedulings() -> Result<()> {
    // RFC-0062 9.2/18: the canonical stdout-then-stderr artifact hash and segment layout must be
    // identical regardless of reader scheduling and chunk sizes.
    let stdout = b"stdout line one\nstdout line two\n";
    let stderr = b"stderr warning\nstderr error detail\n";
    let mut digests = std::collections::BTreeSet::new();
    for chunk in [1usize, 3, 7, 64, 1024] {
        let (_temp, store) = store_fixture()?;
        let config = ProcessStreamCaptureConfigV1 {
            stream_layout: ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
            preview_limit_bytes_per_stream: 64 * 1024,
            artifact_payload_limit_bytes_combined: TOOL_ARTIFACT_MAX_BYTES as u64,
            artifact_reservation_stdout_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
            artifact_reservation_stderr_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
            artifact_staging_limit_bytes_per_stream: TOOL_ARTIFACT_MAX_BYTES as u64,
            observed_limit_bytes_combined: 128 * 1024 * 1024,
        };
        let sink = store.begin_policy_safe_capture(
            "call-dual",
            "shell",
            "text/plain; charset=utf-8",
            ToolArtifactEncoding::Utf8,
            ToolArtifactSensitivity::Ordinary,
        );
        let mut sink = sink.begin_process_capture(config)?;
        for chunk in stdout.chunks(chunk) {
            sink.write_stream(ToolOutputStreamV1::Stdout, chunk)?;
        }
        for chunk in stderr.chunks(chunk) {
            sink.write_stream(ToolOutputStreamV1::Stderr, chunk)?;
        }
        let (descriptor, segments, completeness) = sink.finish_process_capture(
            (stdout.len() + stderr.len()) as u64,
            0,
            ToolSourceCompletenessV1::Complete,
        )?;
        assert_eq!(
            descriptor.observed_bytes,
            (stdout.len() + stderr.len()) as u64
        );
        assert_eq!(descriptor.persisted_bytes, descriptor.observed_bytes);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].stream, ToolOutputStreamV1::Stdout);
        assert_eq!(segments[0].artifact_offset, 0);
        assert_eq!(segments[0].persisted_bytes, stdout.len() as u64);
        assert_eq!(segments[1].stream, ToolOutputStreamV1::Stderr);
        assert_eq!(segments[1].artifact_offset, stdout.len() as u64);
        assert_eq!(segments[1].persisted_bytes, stderr.len() as u64);
        assert_eq!(completeness.storage, ToolStorageCompletenessV1::Complete);
        let body = store.read_all(&descriptor)?;
        let expected = [stdout.as_slice(), stderr.as_slice()].concat();
        assert_eq!(body, expected, "canonical body must be stdout then stderr");
        digests.insert(descriptor.content_sha256.clone());
    }
    assert_eq!(
        digests.len(),
        1,
        "identical stream bytes must produce one canonical hash across chunk schedulings"
    );
    Ok(())
}

#[test]
fn process_capture_redacts_secrets_that_span_chunk_boundaries() -> Result<()> {
    // RFC-0062 10.2: policy redaction runs on the canonical merged bytes, so a secret split
    // across chunk boundaries is still removed deterministically.
    let (_temp, store) = store_fixture()?;
    let config = ProcessStreamCaptureConfigV1 {
        stream_layout: ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
        preview_limit_bytes_per_stream: 64 * 1024,
        artifact_payload_limit_bytes_combined: TOOL_ARTIFACT_MAX_BYTES as u64,
        artifact_reservation_stdout_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
        artifact_reservation_stderr_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
        artifact_staging_limit_bytes_per_stream: TOOL_ARTIFACT_MAX_BYTES as u64,
        observed_limit_bytes_combined: 128 * 1024 * 1024,
    };
    let sink = store.begin_policy_safe_capture(
        "call-secret",
        "shell",
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::Ordinary,
    );
    let mut sink = sink.begin_process_capture(config)?;
    // "token=" and "SUPER-SECRET-123" land in separate chunks.
    sink.write_stream(ToolOutputStreamV1::Stdout, b"prefix token=")?;
    sink.write_stream(ToolOutputStreamV1::Stdout, b"SUPER-SECRET-123 suffix")?;
    let (descriptor, _, completeness) =
        sink.finish_process_capture(31, 0, ToolSourceCompletenessV1::Complete)?;
    let body = String::from_utf8_lossy(&store.read_all(&descriptor)?).into_owned();
    assert!(
        !body.contains("SUPER-SECRET-123"),
        "secret split across chunks must be redacted"
    );
    assert_eq!(completeness.policy, ToolPolicyCompletenessV1::Redacted);
    Ok(())
}

#[test]
fn process_capture_settles_reservations_and_keeps_segment_boundaries_true() -> Result<()> {
    // RFC-0062 9.2/18: stdout 10 MiB + stderr 10 MiB with 8 MiB/8 MiB reservations must settle
    // to a 16 MiB canonical body (8 + 8), and the stderr segment must start exactly where the
    // stdout segment ends — never at a guessed offset.
    let (_temp, store) = store_fixture()?;
    let config = ProcessStreamCaptureConfigV1 {
        stream_layout: ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
        preview_limit_bytes_per_stream: 64 * 1024,
        artifact_payload_limit_bytes_combined: 16 * 1024 * 1024,
        artifact_reservation_stdout_bytes: 8 * 1024 * 1024,
        artifact_reservation_stderr_bytes: 8 * 1024 * 1024,
        artifact_staging_limit_bytes_per_stream: 16 * 1024 * 1024,
        observed_limit_bytes_combined: 128 * 1024 * 1024,
    };
    let sink = store.begin_policy_safe_capture(
        "call-dual-cap",
        "shell",
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::Ordinary,
    );
    let mut sink = sink.begin_process_capture(config)?;
    sink.write_stream(ToolOutputStreamV1::Stdout, &vec![b'a'; 10 * 1024 * 1024])?;
    sink.write_stream(ToolOutputStreamV1::Stderr, &vec![b'b'; 10 * 1024 * 1024])?;
    let (descriptor, segments, completeness) =
        sink.finish_process_capture(20 * 1024 * 1024, 0, ToolSourceCompletenessV1::Complete)?;
    assert_eq!(descriptor.persisted_bytes, 16 * 1024 * 1024);
    assert_eq!(descriptor.observed_bytes, 20 * 1024 * 1024);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].persisted_bytes, 8 * 1024 * 1024);
    assert_eq!(segments[0].artifact_offset, 0);
    assert_eq!(segments[1].persisted_bytes, 8 * 1024 * 1024);
    assert_eq!(segments[1].artifact_offset, 8 * 1024 * 1024);
    assert_eq!(segments[0].eligible_bytes, 10 * 1024 * 1024);
    assert_eq!(segments[1].eligible_bytes, 10 * 1024 * 1024);
    assert_eq!(
        completeness.storage,
        ToolStorageCompletenessV1::TruncatedAtLimit
    );
    let body = store.read_all(&descriptor)?;
    assert_eq!(body.len(), 16 * 1024 * 1024);
    assert!(body[..8 * 1024 * 1024].iter().all(|b| *b == b'a'));
    assert!(body[8 * 1024 * 1024..].iter().all(|b| *b == b'b'));
    // Staging files must not survive finalize.
    let staging = store.root().join("staging");
    assert!(!staging.exists() || std::fs::read_dir(&staging)?.next().is_none());
    Ok(())
}

#[test]
fn process_capture_segment_boundaries_survive_variable_length_redaction() -> Result<()> {
    // RFC-0062 9.2: redaction shortens the stdout stream; the stderr segment must still start
    // exactly after the redacted stdout bytes, never at the pre-redaction offset.
    let (_temp, store) = store_fixture()?;
    let config = ProcessStreamCaptureConfigV1 {
        stream_layout: ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
        preview_limit_bytes_per_stream: 64 * 1024,
        artifact_payload_limit_bytes_combined: 1024 * 1024,
        artifact_reservation_stdout_bytes: 512 * 1024,
        artifact_reservation_stderr_bytes: 512 * 1024,
        artifact_staging_limit_bytes_per_stream: 1024 * 1024,
        observed_limit_bytes_combined: 128 * 1024 * 1024,
    };
    let sink = store.begin_policy_safe_capture(
        "call-redact-segment",
        "shell",
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::Ordinary,
    );
    let mut sink = sink.begin_process_capture(config)?;
    let stdout = format!("head {} tail", "token=SECRET-12345 ".repeat(10_000));
    sink.write_stream(ToolOutputStreamV1::Stdout, stdout.as_bytes())?;
    sink.write_stream(ToolOutputStreamV1::Stderr, b"stderr line")?;
    let (descriptor, segments, _) = sink.finish_process_capture(
        (stdout.len() + 11) as u64,
        0,
        ToolSourceCompletenessV1::Complete,
    )?;
    let body = store.read_all(&descriptor)?;
    let body_text = String::from_utf8_lossy(&body);
    assert!(!body_text.contains("SECRET-12345"));
    assert_eq!(segments[0].artifact_offset, 0);
    assert_eq!(
        segments[1].artifact_offset, segments[0].persisted_bytes,
        "stderr must start exactly after the redacted stdout bytes"
    );
    assert!(
        body_text[segments[1].artifact_offset as usize..].starts_with("stderr line"),
        "stderr segment must contain only stderr bytes"
    );
    assert_eq!(
        segments[1].persisted_bytes as usize + segments[0].persisted_bytes as usize,
        body.len()
    );
    Ok(())
}
