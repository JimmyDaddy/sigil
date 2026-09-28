//! Reuses the A2 causal coverage and price evidence for the opt-in A5 Task experiment.
use super::*;

/// Observes Task experiment usage through the same exact A2 evidence projection.
pub fn observe_task_eval_usage(
    records: Option<&[sigil_kernel::SessionStreamRecord]>,
    session_scope: &str,
    events: &[PublicRunEvent],
) -> serde_json::Value {
    let mut observer = ModelEvalEventRecorder::default();
    for event in events {
        let _ = observer.handle_public_event(event.clone());
    }
    let coverage = records
        .and_then(|records| {
            super::super::trajectory::observe_usage_coverage(
                records,
                session_scope,
                0,
                &observer.usage,
            )
            .ok()
        })
        .unwrap_or_else(|| ModelEvalUsageCoverage {
            observation_error: Some("durable usage association unavailable".into()),
            ..ModelEvalUsageCoverage::default()
        });
    let confidence = match coverage.confidence(&observer.usage) {
        ModelEvalCostConfidence::Reported => "reported",
        ModelEvalCostConfidence::Estimated => "estimated",
        ModelEvalCostConfidence::Unknown => "unknown",
    };
    serde_json::json!({
        "prompt_tokens": observer.usage.prompt_tokens,
        "completion_tokens": observer.usage.completion_tokens,
        "cache_hit_tokens": observer.usage.cache_hit_tokens,
        "cache_miss_tokens": observer.usage.cache_miss_tokens,
        "usage_events": observer.usage.usage_events,
        "known_usage_cost_usd": observer.usage.known_usage_cost_usd(),
        "reported_or_priced_cost_usd": coverage.cost_usd(&observer.usage),
        "cost_confidence": confidence,
        "pricing_snapshot_ids": observer.usage.pricing_snapshot_ids,
        "observed_provider_system_fingerprints": observer.usage.provider_system_fingerprints,
        "actual_response_model_version": serde_json::Value::Null,
        "usage_coverage": coverage,
        "budget_accounting_is_billing": false,
    })
}

/// Bounded observation of original files and the actual final workspace, without granting writes.
/// Unknown reads and non-text changes remain explicit; the patch covers regular file content only.
pub fn observe_task_eval_patch(
    fixture: &MaterializedModelEvalFixture,
) -> (String, serde_json::Value) {
    let mut paths = fixture
        .fixture_files
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut pending = vec![fixture.workspace_root.clone()];
    let mut scan_errors = Vec::new();
    let mut entries = 0_usize;
    // This is only an evidence budget. Exceeding it marks the observation incomplete.
    const MAX_OBSERVED_ENTRIES: usize = 4096;
    'scan: while let Some(directory) = pending.pop() {
        let Ok(children) = fs::read_dir(&directory) else {
            scan_errors.push("directory_unavailable");
            continue;
        };
        for child in children {
            entries += 1;
            if entries > MAX_OBSERVED_ENTRIES {
                scan_errors.push("entry_budget_exceeded");
                break 'scan;
            }
            let Ok(child) = child else {
                scan_errors.push("directory_entry_unavailable");
                continue;
            };
            let path = child.path();
            let Ok(kind) = child.file_type() else {
                scan_errors.push("file_type_unavailable");
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(&fixture.workspace_root) {
                paths.insert(relative.to_path_buf());
            }
        }
    }
    let mut complete = scan_errors.is_empty();
    let mut remaining = super::super::MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES;
    let mut patch = String::new();
    let mut files = Vec::new();
    for path in paths {
        let before = (|| {
            let Some(source) = fixture.fixture_file_sources.get(&path) else {
                return Ok(None);
            };
            let source = super::super::resolve_source_path(&fixture.fixture_source_root, source)?;
            let bytes = super::super::read_bounded_regular_file(
                &source,
                super::super::MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
                "patch source",
            )?;
            super::super::validate_digest(
                "patch source",
                &fixture.fixture_file_digests[&path],
                &bytes,
            )?;
            Ok::<_, anyhow::Error>(Some(bytes))
        })();
        let after = (|| -> Result<Option<Vec<u8>>> {
            match fs::symlink_metadata(fixture.workspace_root.join(&path)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
                Ok(_) => {
                    let source = super::super::resolve_source_path(&fixture.workspace_root, &path)?;
                    let bytes = super::super::read_bounded_regular_file(
                        &source,
                        remaining,
                        "patch result",
                    )?;
                    remaining -= bytes.len() as u64;
                    Ok(Some(bytes))
                }
            }
        })();
        let before_hash = before
            .as_ref()
            .ok()
            .and_then(|bytes| bytes.as_deref())
            .map(sha256_digest);
        let after_hash = after
            .as_ref()
            .ok()
            .and_then(|bytes| bytes.as_deref())
            .map(sha256_digest);
        let (Ok(before), Ok(after)) = (before, after) else {
            complete = false;
            files.push(serde_json::json!({"path":path,"before_sha256":before_hash,
                "after_sha256":after_hash,"status":"unavailable"}));
            continue;
        };
        if before == after {
            continue;
        }
        let text = std::str::from_utf8(before.as_deref().unwrap_or_default())
            .ok()
            .zip(std::str::from_utf8(after.as_deref().unwrap_or_default()).ok());
        let printable_path = path.to_str().filter(|path| {
            !path.chars().any(char::is_control) && !path.contains('"') && !path.contains('\\')
        });
        let status = if let (Some((before_text, after_text)), Some(name)) = (text, printable_path)
            && !before_text.contains('\0')
            && !after_text.contains('\0')
        {
            if before.as_ref().is_none_or(Vec::is_empty) && after.as_ref().is_none_or(Vec::is_empty)
            {
                patch.push_str(&format!(
                    "diff --git a/{name} b/{name}\n{} file mode 100644\n",
                    if before.is_none() { "new" } else { "deleted" }
                ));
            } else {
                patch.push_str(
                    &similar::TextDiff::from_lines(before_text, after_text)
                        .unified_diff()
                        .header(
                            &if before.is_none() {
                                "/dev/null".to_owned()
                            } else {
                                format!("a/{name}")
                            },
                            &if after.is_none() {
                                "/dev/null".to_owned()
                            } else {
                                format!("b/{name}")
                            },
                        )
                        .to_string(),
                );
            }
            "text"
        } else {
            complete = false;
            "binary_or_unrepresentable"
        };
        files.push(serde_json::json!({"path":path,"before_sha256":before_hash,
            "after_sha256":after_hash,"status":status}));
    }
    let observation = serde_json::json!({"path":"actual.patch","sha256":sha256_digest(patch.as_bytes()),
        "complete_for_workspace_regular_file_contents":complete,"mode_changes_observed":false,
        "workspace_scan_errors":scan_errors,"files":files,
        "max_observed_entries":MAX_OBSERVED_ENTRIES,
        "max_result_bytes":super::super::MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES});
    (patch, observation)
}
