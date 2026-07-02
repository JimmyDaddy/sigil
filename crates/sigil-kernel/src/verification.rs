//! RFC-0003 verification contract foundation.
//!
//! This module defines provider-neutral verification state, evidence receipts, workspace snapshot
//! binding, a minimal check runner, and a deterministic readiness reducer. The runner records
//! command/check facts as durable events and projects proof through `VerificationRecorded`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    DurableEventType, EventClass, EventId, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionNetworkReceipt, ExecutionRequest, ExecutionResourceReceipt,
    PluginHookExecutionFinishedEntry, PluginHookExecutionStartedEntry, PluginHookExecutionStatus,
    PluginHookKind, PluginHookOutputEnvelope, Session, SessionId, StoredEvent,
    WorkspaceMutationDetected,
    session::{ControlEntry, SessionLogEntry},
    stable_event_uuid,
};

#[cfg(test)]
#[path = "tests/verification_tests.rs"]
mod tests;

mod config;
mod discovery;
mod evidence;
mod readiness;
mod runner;
mod shared;
mod snapshot;

pub use config::*;
pub use discovery::*;
pub use evidence::*;
pub use readiness::*;
pub use runner::*;
pub use snapshot::*;

use discovery::normalize_check_cwd;
use evidence::receipt_matches_current_context;
#[cfg(test)]
use readiness::finalize_new_run;
use runner::sandbox_profile_hash_for_execution;
#[cfg(test)]
use runner::{CheckCommandOutput, check_failure_reason, sandbox_profile_hash, truncated_lossy};
use shared::{canonical_json_bytes, stable_hash_parts};
#[cfg(test)]
use snapshot::snapshot_entry_for_path;
