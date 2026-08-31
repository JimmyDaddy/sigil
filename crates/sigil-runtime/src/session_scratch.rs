//! Runtime bridge for the authority-owned SessionScratch service.
//!
//! The bridge is the only production adapter that converts authority-private physical results
//! into the narrow built-in-tool scratch provider seam. Built-in tools retain no allocator or
//! cleanup ownership; test-only local providers remain available inside their crate fixtures.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};

use sigil_resource_authority::session_scratch::{
    SessionScratchAuthorityV1, SessionScratchDeleteOutcomeV1, SessionScratchGcConfigV1,
};

#[derive(Debug, Clone)]
struct AuthorityScratchNamespaceProviderV1 {
    authority: Arc<SessionScratchAuthorityV1>,
}

#[derive(Debug)]
struct AuthorityScratchNamespaceLeaseV1 {
    _lease: sigil_resource_authority::session_scratch::SessionScratchLeaseV1,
}

impl sigil_tools_builtin::ScratchNamespaceProviderLease for AuthorityScratchNamespaceLeaseV1 {}

impl sigil_tools_builtin::ScratchNamespaceProvider for AuthorityScratchNamespaceProviderV1 {
    fn acquire(
        &self,
        session_key: &str,
    ) -> Result<Box<dyn sigil_tools_builtin::ScratchNamespaceProviderLease>> {
        Ok(Box::new(AuthorityScratchNamespaceLeaseV1 {
            _lease: self
                .authority
                .acquire(Some(session_key))
                .map_err(authority_error)?,
        }))
    }

    fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> PathBuf {
        self.authority.session_directory(session_scope_id)
    }

    fn ensure_session_scratch(
        &self,
        session_scope_id: Option<&str>,
        quota: &sigil_tools_builtin::ScratchQuota,
    ) -> Result<sigil_tools_builtin::SessionScratchProvision> {
        let provision = self
            .authority
            .ensure(
                session_scope_id,
                quota.per_session_bytes,
                quota.workspace_hard_bytes,
            )
            .map_err(authority_error)?;
        Ok(sigil_tools_builtin::SessionScratchProvision {
            dir: provision.directory,
            usage: sigil_tools_builtin::ScratchUsage {
                session_bytes: provision.usage.session_bytes,
                workspace_bytes: provision.usage.workspace_bytes,
                session_entry_count: provision.usage.session_entry_count,
                workspace_entry_count: provision.usage.workspace_entry_count,
            },
        })
    }

    fn measure_scratch_usage(
        &self,
        session_key: &str,
    ) -> Result<sigil_tools_builtin::ScratchUsage> {
        let usage = self
            .authority
            .measure(session_key)
            .map_err(authority_error)?;
        Ok(sigil_tools_builtin::ScratchUsage {
            session_bytes: usage.session_bytes,
            workspace_bytes: usage.workspace_bytes,
            session_entry_count: usage.session_entry_count,
            workspace_entry_count: usage.workspace_entry_count,
        })
    }

    fn gc_scratch_namespaces(
        &self,
        _control: &sigil_tools_builtin::ScratchNamespaceControl,
        config: &sigil_tools_builtin::ScratchGcConfig,
        now_ms: u64,
    ) -> Result<sigil_tools_builtin::ScratchGcReport> {
        let report = self
            .authority
            .gc(
                SessionScratchGcConfigV1 {
                    ttl_ms: config.ttl_ms,
                    max_entries: 250_000,
                },
                now_ms,
            )
            .map_err(authority_error)?;
        Ok(sigil_tools_builtin::ScratchGcReport {
            scanned: report.scanned,
            deleted: report.deleted,
            skipped_leased: report.skipped_leased,
            skipped_recent: report.skipped_recent,
            skipped_invalid: report.skipped_invalid,
            quarantined: report.quarantined,
            deleted_bytes: report.deleted_bytes,
            workspace_usage_bytes: report.workspace_usage_bytes,
            diagnostics: report.diagnostics,
        })
    }

    fn delete_session_scratch_namespace(
        &self,
        session_scope_id: Option<&str>,
        _control: &sigil_tools_builtin::ScratchNamespaceControl,
    ) -> Result<sigil_tools_builtin::ScratchDeleteOutcome> {
        Ok(
            match self
                .authority
                .delete(session_scope_id)
                .map_err(authority_error)?
            {
                SessionScratchDeleteOutcomeV1::Deleted => {
                    sigil_tools_builtin::ScratchDeleteOutcome::Deleted
                }
                SessionScratchDeleteOutcomeV1::NotPresent => {
                    sigil_tools_builtin::ScratchDeleteOutcome::NotPresent
                }
                SessionScratchDeleteOutcomeV1::SkippedLeased => {
                    sigil_tools_builtin::ScratchDeleteOutcome::SkippedLeased
                }
            },
        )
    }
}

fn authority_error(
    error: sigil_resource_authority::session_scratch::SessionScratchErrorV1,
) -> anyhow::Error {
    match error {
        sigil_resource_authority::session_scratch::SessionScratchErrorV1::QuotaExceeded(
            payload,
        ) => payload.into(),
        sigil_resource_authority::session_scratch::SessionScratchErrorV1::EntryLimitExceeded {
            limit,
            observed,
        } => sigil_tools_builtin::ScratchMeasurementError::EntryLimitExceeded {
            limit,
            observed_entries: observed,
        }
        .into(),
        error => anyhow!(error),
    }
}

/// Constructs the sole production SessionScratch control from the authority-owned physical
/// namespace. The returned control is safe to share across run surfaces in one process.
#[must_use]
pub fn authority_scratch_control(root: PathBuf) -> sigil_tools_builtin::ScratchNamespaceControl {
    sigil_tools_builtin::ScratchNamespaceControl::from_provider(Arc::new(
        AuthorityScratchNamespaceProviderV1 {
            authority: Arc::new(SessionScratchAuthorityV1::new(root)),
        },
    ))
}

#[cfg(test)]
#[path = "tests/session_scratch_tests.rs"]
mod tests;
