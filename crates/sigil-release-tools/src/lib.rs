//! RFC-0071 section 9.5 / R71.4: nonshipping release-owner tools.
//!
//! This package is publish=false and must never be a normal/build/dev dependency of any shipping
//! root. It owns the three release-owner commands (model-eval, model-eval-route-contract,
//! model-eval-rollout-manifest) removed from the shipping `sigil` binary.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use sigil_resource_authority::release_output::{
    AuthorityBorrowedReleaseOutputServiceV1, BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
    BorrowedReleaseOutputEntryV1, BorrowedReleaseOutputOperationV1, BorrowedReleaseOutputRequestV1,
    BorrowedReleaseOutputServiceV1,
};

/// Release-tool-owned output publisher. The runtime receives only this narrow port; the
/// authority service fixes the invocation root and returns closed receipts for every write.
#[derive(Debug)]
pub struct ReleaseOutputOwnerV1 {
    service: Arc<AuthorityBorrowedReleaseOutputServiceV1>,
}

impl ReleaseOutputOwnerV1 {
    pub fn new(output_root: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(AuthorityBorrowedReleaseOutputServiceV1::new(output_root)),
        }
    }

    pub fn publish_file(&self, destination: &Path, content: &[u8]) -> anyhow::Result<()> {
        self.service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: sigil_kernel::resource::OpaqueRegistrationCapsuleId::new(format!(
                    "release-file-{}",
                    uuid::Uuid::new_v4()
                )),
                operation: BorrowedReleaseOutputOperationV1::File,
                destination: destination.to_owned(),
                content: content.to_vec(),
                entries: Vec::new(),
            })
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!(error))
    }

    pub fn publish_tree(
        &self,
        destination: &Path,
        entries: &[(PathBuf, Vec<u8>)],
    ) -> anyhow::Result<()> {
        self.service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: sigil_kernel::resource::OpaqueRegistrationCapsuleId::new(format!(
                    "release-tree-{}",
                    uuid::Uuid::new_v4()
                )),
                operation: BorrowedReleaseOutputOperationV1::Tree,
                destination: destination.to_owned(),
                content: Vec::new(),
                entries: entries
                    .iter()
                    .map(|(relative_path, content)| BorrowedReleaseOutputEntryV1 {
                        relative_path: relative_path.clone(),
                        content: content.clone(),
                    })
                    .collect(),
            })
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!(error))
    }
}

impl sigil_runtime::model_eval::ReleaseOutputOwnerV1 for ReleaseOutputOwnerV1 {
    fn prepare_tree_root(&self, root: &Path) -> anyhow::Result<()> {
        self.service
            .prepare_tree_root(root)
            .map_err(|error| anyhow::anyhow!(error))
    }

    fn publish_file(&self, destination: &Path, content: &[u8]) -> anyhow::Result<()> {
        Self::publish_file(self, destination, content)
    }

    fn publish_tree(
        &self,
        destination: &Path,
        entries: &[(PathBuf, Vec<u8>)],
    ) -> anyhow::Result<()> {
        Self::publish_tree(self, destination, entries)
    }
}

pub const FROZEN_ORCHESTRATION_CASE_COUNT: usize = 50;

/// Resolves safe relative fixture roots for a model-eval campaign (never accepts traversal).
pub fn resolve_model_eval_fixture_roots(
    launch_cwd: &Path,
    cases: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    if cases.is_empty() {
        anyhow::bail!("model eval requires at least one --case");
    }
    let fixture_root = launch_cwd.join("dev/evals/model-fixtures");
    let mut fixture_roots = Vec::new();
    for case in cases {
        if case == "orchestration-v1" {
            fixture_roots.extend(resolve_frozen_orchestration_fixture_roots(&fixture_root)?);
            continue;
        }
        let relative = Path::new(case);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("model eval case must be a safe relative fixture path: {case}");
        }
        fixture_roots.push(fixture_root.join(relative));
    }
    Ok(fixture_roots)
}

/// Frozen orchestration corpus resolution (exact count, no symlinks).
pub fn resolve_frozen_orchestration_fixture_roots(
    fixture_root: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let corpus_root = fixture_root.join("orchestration-v1");
    let mut fixture_roots = Vec::new();
    for case_class in ["negative", "positive"] {
        let class_root = corpus_root.join(case_class);
        let metadata = std::fs::symlink_metadata(&class_root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!(
                "orchestration-v1 case class must be a regular directory: {}",
                class_root.display()
            );
        }
        for entry in std::fs::read_dir(&class_root)? {
            let entry = entry?;
            let metadata = entry.file_type()?;
            if metadata.is_symlink() || !metadata.is_dir() {
                anyhow::bail!(
                    "orchestration-v1 case must be a regular directory: {}",
                    entry.path().display()
                );
            }
            fixture_roots.push(entry.path());
        }
    }
    fixture_roots.sort();
    if fixture_roots.len() != FROZEN_ORCHESTRATION_CASE_COUNT {
        anyhow::bail!(
            "orchestration-v1 corpus must contain exactly {FROZEN_ORCHESTRATION_CASE_COUNT} cases, found {}",
            fixture_roots.len()
        );
    }
    Ok(fixture_roots)
}

/// Validates a model-eval report manifest (acceptance gate: exact repetition counts).
pub fn validate_model_eval_manifest(
    manifest: &sigil_kernel::ModelEvalReportManifestV3,
) -> anyhow::Result<()> {
    if manifest.requested_repetitions == 0
        || manifest.provider_admitted_repetitions != manifest.requested_repetitions
        || manifest.completed_repetitions != manifest.requested_repetitions
        || manifest.skipped_repetitions != 0
        || manifest.accepted_repetitions != manifest.requested_repetitions
    {
        anyhow::bail!(
            "model eval acceptance failed: requested {}, provider-admitted {}, completed {}, skipped {}, accepted {}",
            manifest.requested_repetitions,
            manifest.provider_admitted_repetitions,
            manifest.completed_repetitions,
            manifest.skipped_repetitions,
            manifest.accepted_repetitions,
        );
    }
    Ok(())
}

/// Validates an orchestration eval manifest (acceptance gate: all route gates qualified).
pub fn validate_orchestration_eval_manifest(
    manifest: &sigil_kernel::OrchestrationEvalReportManifestV1,
) -> anyhow::Result<()> {
    if manifest.route_gates.is_empty() {
        anyhow::bail!("orchestration eval acceptance failed: no route gates were produced");
    }
    let rejected = manifest
        .route_gates
        .iter()
        .filter(|gate| gate.status != sigil_kernel::OrchestrationEvalRouteStatus::Qualified)
        .map(|gate| {
            format!(
                "{}={:?} ({})",
                gate.identity_digest,
                gate.status,
                gate.reasons.join("; ")
            )
        })
        .collect::<Vec<_>>();
    if !rejected.is_empty() {
        anyhow::bail!(
            "orchestration eval acceptance failed: {}",
            rejected.join(", ")
        );
    }
    Ok(())
}

/// Parses --max-cost-usd with bounded positive range semantics.
pub fn parse_model_eval_cost_microusd(raw: &str) -> anyhow::Result<u64> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("--max-cost-usd must be a positive decimal"))?;
    if !value.is_finite() || value <= 0.0 || value > u64::MAX as f64 / 1_000_000.0 {
        anyhow::bail!("--max-cost-usd is outside the supported positive range");
    }
    let microusd = (value * 1_000_000.0) as u64;
    if microusd == 0 {
        anyhow::bail!("--max-cost-usd is below one micro-usd resolution");
    }
    Ok(microusd)
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
