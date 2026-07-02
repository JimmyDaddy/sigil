use super::*;

pub(super) fn render_mutation_artifact_retention_summary(
    config_state: &ConfigState,
    preview: &MutationArtifactRetentionPreview,
) -> Vec<String> {
    let retention = &config_state
        .draft
        .base_root_config
        .storage
        .mutation_artifact_retention;
    let mut lines = vec![
        render_config_readonly_row(
            "Max artifacts",
            &optional_count_summary(retention.max_artifacts),
        ),
        render_config_readonly_row("Max bytes", &optional_bytes_summary(retention.max_bytes)),
        render_config_readonly_row(
            "Expire older than",
            &optional_duration_ms_summary(retention.expire_older_than_ms),
        ),
    ];
    match preview {
        MutationArtifactRetentionPreview::Pending => {
            lines.push(render_config_readonly_row("Preview", "pending"));
        }
        MutationArtifactRetentionPreview::Ready { report, artifacts } => {
            lines.push(render_config_readonly_row(
                "Current artifacts",
                &format!(
                    "{} ({})",
                    report.scanned_artifacts,
                    optional_bytes_summary(Some(report.scanned_bytes))
                ),
            ));
            lines.push(render_config_readonly_row(
                "Cleanup preview",
                &format!(
                    "expire {}, delete {}, unavailable {}",
                    report.expired_artifacts,
                    report.deleted_artifacts,
                    report.unavailable_artifacts
                ),
            ));
            if report.has_cleanup_candidates() {
                lines.push(render_config_readonly_row(
                    "Maintenance",
                    &format!(
                        "clean recommended ({} artifacts, {})",
                        report.cleanup_candidate_artifacts(),
                        optional_bytes_summary(Some(report.cleanup_candidate_bytes()))
                    ),
                ));
            }
            lines.push(render_config_readonly_row(
                "Cleanup bytes",
                &format!(
                    "expire {}, delete {}",
                    optional_bytes_summary(Some(report.expired_bytes)),
                    optional_bytes_summary(Some(report.deleted_bytes))
                ),
            ));
            lines.extend(render_mutation_artifact_inventory_summary(
                artifacts,
                config_state.selected_storage_artifact_index,
            ));
            lines.extend(render_selected_mutation_artifact_detail(
                artifacts,
                config_state.selected_storage_artifact_index,
            ));
        }
        MutationArtifactRetentionPreview::Unavailable(error) => {
            lines.push(render_config_readonly_row("Preview", "unavailable"));
            lines.push(render_config_hint_row(&truncate_config_detail(error, 72)));
        }
    }
    lines
}

pub(super) fn render_mutation_artifact_inventory_summary(
    artifacts: &[sigil_kernel::MutationArtifactInventoryItem],
    selected_index: usize,
) -> Vec<String> {
    const MAX_ARTIFACT_ROWS: usize = 3;
    if artifacts.is_empty() {
        return vec![render_config_hint_row("No mutation artifacts found")];
    }
    let mut lines = vec!["[artifact list]".to_owned()];
    lines.extend(
        artifacts
            .iter()
            .enumerate()
            .take(MAX_ARTIFACT_ROWS)
            .map(|(index, artifact)| {
                render_mutation_artifact_inventory_row(artifact, index == selected_index)
            }),
    );
    let hidden = artifacts.len().saturating_sub(MAX_ARTIFACT_ROWS);
    if hidden > 0 {
        lines.push(format!("... {hidden} more mutation artifacts"));
    }
    lines
}

pub(super) fn render_mutation_artifact_inventory_row(
    artifact: &sigil_kernel::MutationArtifactInventoryItem,
    selected: bool,
) -> String {
    let source = artifact_source_summary(artifact);
    let status = if artifact.blob_available {
        "available"
    } else {
        "unavailable"
    };
    let marker = if selected { ">" } else { "-" };
    format!(
        "{marker} {} · {} · {}",
        source,
        optional_bytes_summary(Some(artifact.size)),
        status
    )
}

pub(super) fn render_selected_mutation_artifact_detail(
    artifacts: &[sigil_kernel::MutationArtifactInventoryItem],
    selected_index: usize,
) -> Vec<String> {
    let Some(artifact) = artifacts.get(selected_index.min(artifacts.len().saturating_sub(1)))
    else {
        return Vec::new();
    };
    let availability = if artifact.blob_available {
        "available"
    } else {
        "unavailable"
    };
    let mut lines = vec![
        String::new(),
        "[selected artifact]".to_owned(),
        render_config_readonly_row(
            "Selected",
            &format!(
                "{} of {}",
                selected_index.min(artifacts.len().saturating_sub(1)) + 1,
                artifacts.len()
            ),
        ),
        render_config_readonly_row("Size", &optional_bytes_summary(Some(artifact.size))),
        render_config_readonly_row("Availability", availability),
        render_config_readonly_row(
            "Restore impact",
            if artifact.blob_available {
                "snapshot content available"
            } else {
                "snapshot content unavailable"
            },
        ),
    ];
    if artifact.source_paths.is_empty() {
        lines.push(render_config_readonly_row("Source count", "0"));
    } else {
        for (index, source_path) in artifact.source_paths.iter().take(3).enumerate() {
            push_wrapped_readonly_rows(
                &mut lines,
                &format!("Source {}", index + 1),
                &source_path.display().to_string(),
            );
        }
        if artifact.source_paths.len() > 3 {
            lines.push(format!(
                "... {} more artifact sources",
                artifact.source_paths.len() - 3
            ));
        }
    }
    lines
}

pub(super) fn artifact_source_summary(
    artifact: &sigil_kernel::MutationArtifactInventoryItem,
) -> String {
    let Some(first) = artifact.source_paths.first() else {
        return "unknown source".to_owned();
    };
    let first = truncate_config_detail(&first.display().to_string(), 28);
    let hidden = artifact.source_paths.len().saturating_sub(1);
    if hidden == 0 {
        first
    } else {
        format!("{first} +{hidden}")
    }
}

pub(super) fn optional_count_summary(value: Option<usize>) -> String {
    value
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unlimited".to_owned())
}

pub(super) fn optional_bytes_summary(value: Option<u64>) -> String {
    let Some(bytes) = value else {
        return "unlimited".to_owned();
    };
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB && bytes % GIB == 0 {
        return format!("{} GiB", bytes / GIB);
    }
    if bytes >= MIB && bytes % MIB == 0 {
        return format!("{} MiB", bytes / MIB);
    }
    format!("{bytes} bytes")
}

pub(super) fn optional_duration_ms_summary(value: Option<u64>) -> String {
    let Some(ms) = value else {
        return "never".to_owned();
    };
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    const HOUR_MS: u64 = 60 * 60 * 1000;
    const MINUTE_MS: u64 = 60 * 1000;
    if ms >= DAY_MS && ms % DAY_MS == 0 {
        let days = ms / DAY_MS;
        return format!("{} {}", days, if days == 1 { "day" } else { "days" });
    }
    if ms >= HOUR_MS && ms % HOUR_MS == 0 {
        let hours = ms / HOUR_MS;
        return format!("{} {}", hours, if hours == 1 { "hour" } else { "hours" });
    }
    if ms >= MINUTE_MS && ms % MINUTE_MS == 0 {
        let minutes = ms / MINUTE_MS;
        return format!(
            "{} {}",
            minutes,
            if minutes == 1 { "minute" } else { "minutes" }
        );
    }
    format!("{ms} ms")
}
