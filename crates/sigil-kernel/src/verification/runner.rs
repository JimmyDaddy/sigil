use super::*;

/// Request for executing one trusted verification check.
#[derive(Debug, Clone)]
pub struct VerificationCheckRunRequest {
    pub workspace_root: PathBuf,
    pub scope: EvidenceScope,
    pub trusted_check: TrustedCheckSpec,
    pub policy: VerificationPolicy,
    pub policy_hash: Option<PolicyHash>,
    pub workspace_trust: WorkspaceTrust,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub workspace_trust_approval_event_id: Option<EventId>,
    pub workspace_trust_sandbox_decision_id: Option<EventId>,
}

/// Request for binding a trusted plugin verification hook result to normal verification evidence.
#[derive(Debug, Clone)]
pub struct PluginVerificationHookReceiptRequest {
    pub workspace_root: PathBuf,
    pub scope: EvidenceScope,
    pub trusted_check: TrustedCheckSpec,
    pub policy: VerificationPolicy,
    pub policy_hash: Option<PolicyHash>,
    pub workspace_trust: WorkspaceTrust,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub workspace_trust_approval_event_id: Option<EventId>,
    pub workspace_trust_sandbox_decision_id: Option<EventId>,
    pub started: PluginHookExecutionStartedEntry,
    pub finished: PluginHookExecutionFinishedEntry,
    pub output: PluginHookOutputEnvelope,
    pub workspace_mutation_event_id: Option<EventId>,
}

/// Executes a trusted verification check and returns the durable verification projection entry.
///
/// The command result is never treated as proof by itself. The returned receipt is bound to a
/// verification-scope workspace snapshot, and a check that mutates that scope is recorded as
/// non-final evidence so the reducer can require a non-writing rerun.
///
/// # Errors
///
/// Returns an error when the workspace cannot be snapshotted, the durable check/command facts
/// cannot be recorded, or the configured command cannot be spawned.
pub async fn run_verification_check(
    session: &mut Session,
    execution_backend: &dyn ExecutionBackend,
    request: VerificationCheckRunRequest,
) -> Result<VerificationRecordedEntry> {
    let workspace_root = fs::canonicalize(&request.workspace_root).with_context(|| {
        format!(
            "failed to canonicalize verification workspace {}",
            request.workspace_root.display()
        )
    })?;
    let workspace_id = stable_workspace_id(&workspace_root)?;
    let check = &request.trusted_check.check_spec;
    let approval_event_id = request
        .workspace_trust_approval_event_id
        .clone()
        .or_else(|| request.trusted_check.approval_event_id.clone());
    let sandbox_decision_id = request
        .workspace_trust_sandbox_decision_id
        .clone()
        .or_else(|| request.trusted_check.sandbox_decision_id.clone());
    if !request.policy.workspace_trust_requirement.is_satisfied(
        request.workspace_trust,
        approval_event_id.as_ref(),
        sandbox_decision_id.as_ref(),
    ) {
        bail!(
            "verification check {} cannot run until workspace trust requirement is satisfied",
            check.check_spec_id
        );
    }
    let before_snapshot = build_workspace_snapshot(
        &workspace_root,
        workspace_id.clone(),
        &request.policy.verification_scope,
        0,
    )?;

    let started_at = Instant::now();
    let command_output = execute_check_command(
        execution_backend,
        &workspace_root,
        &check.command,
        request.policy.timeout_ms,
    )
    .await?;
    let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    let command_event =
        append_command_finished_event(session, check, &request.scope, &command_output, elapsed_ms)?;
    let after_snapshot = build_workspace_snapshot(
        &workspace_root,
        workspace_id.clone(),
        &request.policy.verification_scope,
        0,
    )?;
    let mutates_verification_scope = check.effect.may_mutate_workspace()
        || before_snapshot.workspace_snapshot_id != after_snapshot.workspace_snapshot_id
        || before_snapshot.workspace_knowledge.is_unknown_dirty()
        || after_snapshot.workspace_knowledge.is_unknown_dirty();
    let mutation_event = if mutates_verification_scope {
        append_check_workspace_mutation_detected_event(
            session,
            check,
            &request.scope,
            command_event.as_ref(),
            &workspace_id,
            &before_snapshot,
            &after_snapshot,
        )?
    } else {
        None
    };
    let check_status = check_receipt_status(&command_output, mutates_verification_scope);
    let failure_reason = check_failure_reason(&command_output, request.policy.timeout_ms);
    let check_event = append_check_finished_event(
        session,
        check,
        &request.scope,
        command_event.as_ref(),
        &before_snapshot,
        &after_snapshot,
        check_status,
        mutates_verification_scope,
        mutation_event.as_ref(),
    )?;
    let (source_session_id, source_event_id, recorded_at_stream_sequence) =
        check_event_identity(session, check, &request.scope, check_event.as_ref());
    let current_snapshot_id = after_snapshot
        .workspace_snapshot_id
        .clone()
        .unwrap_or_else(|| {
            stable_event_uuid(
                "sigil-verification-incomplete-snapshot",
                &format!(
                    "{}:{}:{}",
                    source_session_id, source_event_id, recorded_at_stream_sequence
                ),
            )
        });
    let receipt_id = stable_event_uuid(
        "sigil-verification-receipt",
        &format!(
            "{}:{}:{}:{}",
            source_session_id, source_event_id, check.check_spec_id, current_snapshot_id
        ),
    );
    let verification_receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id,
            source_session_id,
            source_event_id,
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope: request.scope,
            producer_tool_call: None,
            workspace_revision: Some(0),
            workspace_snapshot_id: Some(current_snapshot_id.clone()),
            policy_hash: request.policy_hash,
            changeset_id: None,
            status: check_status,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence,
        },
        binding: VerificationBinding {
            workspace_id,
            workspace_snapshot_id: current_snapshot_id,
            verification_scope_hash: request.policy.verification_scope.scope_hash,
            check_spec_hash: check.check_spec_hash.clone(),
            environment_fingerprint: environment_fingerprint(check),
            sandbox_profile_hash: sandbox_profile_hash_for_execution(
                request.policy.sandbox_profile,
                command_output.backend,
                command_output.backend_capabilities,
                &command_output.network,
            ),
            execution_backend: Some(command_output.backend),
            execution_backend_capabilities: Some(command_output.backend_capabilities),
            execution_network: command_output.network.clone(),
            workspace_trust_snapshot_id: request.workspace_trust_snapshot_id,
            approval_event_id,
            sandbox_decision_id,
        },
        check_spec_id: check.check_spec_id.clone(),
        check_status,
        failure_reason,
        mutates_verification_scope,
    };
    Ok(VerificationRecordedEntry {
        receipt: verification_receipt,
    })
}

/// Converts a trusted plugin verification hook execution into normal verification evidence.
///
/// The hook stdout is treated as data, not as a verdict authority. The returned receipt is bound to
/// the current verification-scope workspace snapshot, the check spec hash, and the execution
/// backend/capability receipt emitted by the hook process.
///
/// # Errors
///
/// Returns an error when the hook evidence is inconsistent, the hook is not a verification hook,
/// workspace trust policy is unsatisfied, or the workspace snapshot cannot be built.
pub fn record_plugin_verification_hook_receipt(
    session: &mut Session,
    request: PluginVerificationHookReceiptRequest,
) -> Result<VerificationRecordedEntry> {
    validate_plugin_verification_hook_evidence(
        &request.started,
        &request.finished,
        &request.output,
    )?;
    let workspace_root = fs::canonicalize(&request.workspace_root).with_context(|| {
        format!(
            "failed to canonicalize plugin verification workspace {}",
            request.workspace_root.display()
        )
    })?;
    let workspace_id = stable_workspace_id(&workspace_root)?;
    let check = &request.trusted_check.check_spec;
    let approval_event_id = request
        .workspace_trust_approval_event_id
        .clone()
        .or_else(|| request.trusted_check.approval_event_id.clone());
    let sandbox_decision_id = request
        .workspace_trust_sandbox_decision_id
        .clone()
        .or_else(|| request.trusted_check.sandbox_decision_id.clone());
    if !request.policy.workspace_trust_requirement.is_satisfied(
        request.workspace_trust,
        approval_event_id.as_ref(),
        sandbox_decision_id.as_ref(),
    ) {
        bail!(
            "plugin verification hook {} cannot record receipt until workspace trust requirement is satisfied",
            request.started.hook_id
        );
    }
    let snapshot = build_workspace_snapshot(
        &workspace_root,
        workspace_id.clone(),
        &request.policy.verification_scope,
        0,
    )?;
    let mutates_verification_scope = request.started.declared_effect.may_mutate_workspace()
        || request.workspace_mutation_event_id.is_some()
        || snapshot.workspace_knowledge.is_unknown_dirty();
    let check_status =
        plugin_hook_receipt_status(request.finished.status, mutates_verification_scope);
    let failure_reason = plugin_hook_failure_reason(&request.finished);
    let check_event = append_plugin_check_finished_event(
        session,
        check,
        &request,
        &snapshot,
        check_status,
        mutates_verification_scope,
    )?;
    let (source_session_id, source_event_id, recorded_at_stream_sequence) =
        check_event_identity(session, check, &request.scope, check_event.as_ref());
    let current_snapshot_id = snapshot.workspace_snapshot_id.clone().unwrap_or_else(|| {
        stable_event_uuid(
            "sigil-plugin-verification-incomplete-snapshot",
            &format!(
                "{}:{}:{}",
                source_session_id, source_event_id, recorded_at_stream_sequence
            ),
        )
    });
    let receipt_id = stable_event_uuid(
        "sigil-plugin-verification-receipt",
        &format!(
            "{}:{}:{}:{}:{}",
            source_session_id,
            source_event_id,
            request.started.plugin_id,
            request.started.hook_id,
            current_snapshot_id
        ),
    );
    let artifact_refs = request
        .output
        .artifact_refs
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<Vec<_>>();
    let verification_receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id,
            source_session_id,
            source_event_id,
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope: request.scope,
            producer_tool_call: Some(request.started.execution_id.clone()),
            workspace_revision: Some(0),
            workspace_snapshot_id: Some(current_snapshot_id.clone()),
            policy_hash: request.policy_hash,
            changeset_id: None,
            status: check_status,
            artifact_refs,
            redaction_state: request.output.redaction_state,
            recorded_at_stream_sequence,
        },
        binding: VerificationBinding {
            workspace_id,
            workspace_snapshot_id: current_snapshot_id,
            verification_scope_hash: request.policy.verification_scope.scope_hash,
            check_spec_hash: check.check_spec_hash.clone(),
            environment_fingerprint: environment_fingerprint(check),
            sandbox_profile_hash: sandbox_profile_hash_for_execution(
                request.policy.sandbox_profile,
                request.finished.backend,
                request.finished.backend_capabilities,
                &request.finished.network,
            ),
            execution_backend: Some(request.finished.backend),
            execution_backend_capabilities: Some(request.finished.backend_capabilities),
            execution_network: request.finished.network.clone(),
            workspace_trust_snapshot_id: request.workspace_trust_snapshot_id,
            approval_event_id,
            sandbox_decision_id,
        },
        check_spec_id: check.check_spec_id.clone(),
        check_status,
        failure_reason,
        mutates_verification_scope,
    };
    Ok(VerificationRecordedEntry {
        receipt: verification_receipt,
    })
}

fn validate_plugin_verification_hook_evidence(
    started: &PluginHookExecutionStartedEntry,
    finished: &PluginHookExecutionFinishedEntry,
    output: &PluginHookOutputEnvelope,
) -> Result<()> {
    if started.hook_kind != PluginHookKind::Verification {
        bail!(
            "plugin hook {} is {:?}, not verification",
            started.hook_id,
            started.hook_kind
        );
    }
    if started.execution_id != finished.execution_id
        || started.plugin_id != finished.plugin_id
        || started.manifest_hash != finished.manifest_hash
        || started.capability_digest != finished.capability_digest
        || started.hook_id != finished.hook_id
        || started.hook_kind != finished.hook_kind
    {
        bail!("plugin verification hook started/finished evidence mismatch");
    }
    if output.execution_id != finished.execution_id
        || output.plugin_id != finished.plugin_id
        || output.hook_id != finished.hook_id
    {
        bail!("plugin verification hook output evidence mismatch");
    }
    Ok(())
}

fn plugin_hook_receipt_status(
    status: PluginHookExecutionStatus,
    mutates_verification_scope: bool,
) -> ReceiptStatus {
    match status {
        PluginHookExecutionStatus::Succeeded if mutates_verification_scope => {
            ReceiptStatus::Inconclusive
        }
        PluginHookExecutionStatus::Succeeded => ReceiptStatus::Succeeded,
        PluginHookExecutionStatus::Failed | PluginHookExecutionStatus::TimedOut => {
            ReceiptStatus::Failed
        }
    }
}

fn plugin_hook_failure_reason(finished: &PluginHookExecutionFinishedEntry) -> Option<String> {
    match finished.status {
        PluginHookExecutionStatus::Succeeded => None,
        PluginHookExecutionStatus::Failed => Some(match finished.exit_code {
            Some(code) => format!("plugin verification hook exited with code {code}"),
            None => "plugin verification hook terminated without exit code".to_owned(),
        }),
        PluginHookExecutionStatus::TimedOut => {
            Some("plugin verification hook timed out".to_owned())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CheckCommandOutput {
    pub(super) backend: ExecutionBackendKind,
    pub(super) backend_capabilities: ExecutionBackendCapabilities,
    pub(super) network: ExecutionNetworkReceipt,
    pub(super) resources: ExecutionResourceReceipt,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) timed_out: bool,
    pub(super) termination: ExecutionTerminationCause,
}

impl CheckCommandOutput {
    fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && matches!(self.termination, ExecutionTerminationCause::Exited)
    }
}

async fn execute_check_command(
    execution_backend: &dyn ExecutionBackend,
    workspace_root: &Path,
    command: &CheckCommand,
    timeout_ms: Option<u64>,
) -> Result<CheckCommandOutput> {
    let cwd = command
        .cwd
        .as_ref()
        .map(|cwd| workspace_root.join(cwd))
        .unwrap_or_else(|| workspace_root.to_path_buf());
    let request = ExecutionRequest {
        program: command.command.clone(),
        args: command.args.clone(),
        cwd: cwd.clone(),
        env: BTreeMap::new(),
        environment_policy: crate::process_environment::ProcessEnvironmentPolicy::InheritParent,
        timeout_ms,
        timeout_secs: timeout_ms
            .map(|timeout_ms| timeout_ms.saturating_add(999) / 1000)
            .unwrap_or(0),
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    };
    let receipt = execution_backend.execute(request).await.with_context(|| {
        format!(
            "failed to spawn verification check {} in {}",
            format_check_command(command),
            cwd.display()
        )
    })?;
    let output = receipt.effective_output();
    Ok(CheckCommandOutput {
        backend: receipt.backend,
        backend_capabilities: receipt.capabilities,
        network: receipt.network,
        resources: receipt.resources,
        exit_code: receipt.exit_code,
        stdout: truncated_captured_lossy(&receipt.stdout, &output.stdout),
        stderr: truncated_captured_lossy(&receipt.stderr, &output.stderr),
        timed_out: matches!(output.termination, ExecutionTerminationCause::TimedOut),
        termination: output.termination,
    })
}

fn check_receipt_status(
    command_output: &CheckCommandOutput,
    mutates_verification_scope: bool,
) -> ReceiptStatus {
    if command_output.succeeded() {
        if mutates_verification_scope {
            ReceiptStatus::Inconclusive
        } else {
            ReceiptStatus::Succeeded
        }
    } else {
        ReceiptStatus::Failed
    }
}

pub(super) fn check_failure_reason(
    command_output: &CheckCommandOutput,
    timeout_ms: Option<u64>,
) -> Option<String> {
    if command_output.succeeded() {
        return None;
    }
    match &command_output.termination {
        ExecutionTerminationCause::TimedOut => {
            return Some(match timeout_ms {
                Some(timeout_ms) => format!("check timed out after {timeout_ms} ms"),
                None => "check timed out".to_owned(),
            });
        }
        ExecutionTerminationCause::OutputLimit {
            stream,
            limit_bytes,
            observed_bytes,
        } => {
            return Some(format!(
                "check exceeded the {} output limit of {limit_bytes} bytes after observing {observed_bytes} bytes",
                stream.as_str()
            ));
        }
        ExecutionTerminationCause::ReaderFailed { stream, reason } => {
            return Some(format!(
                "check {} output reader failed: {reason}",
                stream.as_str()
            ));
        }
        ExecutionTerminationCause::Cancelled => {
            return Some("check interrupted by run cancellation".to_owned());
        }
        ExecutionTerminationCause::Exited => {}
    }
    Some(match command_output.exit_code {
        Some(code) => format!("check exited with code {code}"),
        None => "check terminated without exit code".to_owned(),
    })
}

fn append_command_finished_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_output: &CheckCommandOutput,
    elapsed_ms: u64,
) -> Result<Option<StoredEvent>> {
    session.append_durable_event(
        DurableEventType::CommandFinished,
        EventClass::Critical,
        serde_json::json!({
            "scope": scope,
            "check_spec_id": check.check_spec_id,
            "check_spec_hash": check.check_spec_hash,
            "command": check.command.command,
            "args": check.command.args,
            "cwd": check.command.cwd,
            "exit_code": command_output.exit_code,
            "timed_out": command_output.timed_out,
            "termination": command_output.termination,
            "elapsed_ms": elapsed_ms,
            "execution_backend": command_output.backend,
            "execution_backend_capabilities": command_output.backend_capabilities,
            "execution_network": command_output.network,
            "execution_resources": command_output.resources,
            "stdout_preview": command_output.stdout,
            "stderr_preview": command_output.stderr,
        }),
    )
}

fn append_check_workspace_mutation_detected_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_event: Option<&StoredEvent>,
    workspace_id: &str,
    before_snapshot: &WorkspaceSnapshotBuild,
    after_snapshot: &WorkspaceSnapshotBuild,
) -> Result<Option<StoredEvent>> {
    let (reason, unknown_dirty) = if before_snapshot.workspace_knowledge.is_unknown_dirty() {
        ("snapshot_incomplete_before", true)
    } else if after_snapshot.workspace_knowledge.is_unknown_dirty() {
        ("snapshot_incomplete_after", true)
    } else if before_snapshot.workspace_snapshot_id != after_snapshot.workspace_snapshot_id {
        ("snapshot_changed", false)
    } else {
        ("declared_write_effect", true)
    };
    let seed = format!(
        "{scope:?}:{}:{}:{:?}:{:?}",
        check.check_spec_hash,
        command_event
            .map(|event| event.event_id.as_str())
            .unwrap_or("in-memory"),
        before_snapshot.workspace_snapshot_id,
        after_snapshot.workspace_snapshot_id,
    );
    let operation_id = stable_event_uuid("sigil-verification-mutation", &seed);
    session.append_durable_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::json!({
            "operation_id": operation_id,
            "tool_call_id": null,
            "tool_name": format!("verification_check:{}", check.check_spec_id),
            "tool_effect": check.effect,
            "workspace_id": workspace_id,
            "scope_hash": check.verification_scope_hash,
            "from_workspace_snapshot_id": before_snapshot.workspace_snapshot_id,
            "to_workspace_snapshot_id": after_snapshot.workspace_snapshot_id,
            "base_workspace_revision": 0,
            "workspace_revision": 1,
            "reason": reason,
            "unknown_dirty": unknown_dirty,
        }),
    )
}

fn append_check_finished_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_event: Option<&StoredEvent>,
    before_snapshot: &WorkspaceSnapshotBuild,
    after_snapshot: &WorkspaceSnapshotBuild,
    status: ReceiptStatus,
    mutates_verification_scope: bool,
    mutation_event: Option<&StoredEvent>,
) -> Result<Option<StoredEvent>> {
    session.append_durable_event(
        DurableEventType::CheckFinished,
        EventClass::Critical,
        serde_json::json!({
            "scope": scope,
            "check_spec_id": check.check_spec_id,
            "check_spec_hash": check.check_spec_hash,
            "command_event_id": command_event.map(|event| event.event_id.as_str()),
            "before_workspace_snapshot_id": before_snapshot.workspace_snapshot_id,
            "after_workspace_snapshot_id": after_snapshot.workspace_snapshot_id,
            "before_workspace_knowledge": before_snapshot.workspace_knowledge,
            "after_workspace_knowledge": after_snapshot.workspace_knowledge,
            "status": status,
            "mutates_verification_scope": mutates_verification_scope,
            "workspace_mutation_detected_event_id": mutation_event.map(|event| event.event_id.as_str()),
        }),
    )
}

fn append_plugin_check_finished_event(
    session: &mut Session,
    check: &CheckSpec,
    request: &PluginVerificationHookReceiptRequest,
    snapshot: &WorkspaceSnapshotBuild,
    status: ReceiptStatus,
    mutates_verification_scope: bool,
) -> Result<Option<StoredEvent>> {
    session.append_durable_event(
        DurableEventType::CheckFinished,
        EventClass::Critical,
        serde_json::json!({
            "scope": request.scope,
            "check_spec_id": check.check_spec_id,
            "check_spec_hash": check.check_spec_hash,
            "source": "plugin_hook",
            "plugin_id": request.started.plugin_id,
            "hook_id": request.started.hook_id,
            "hook_kind": request.started.hook_kind,
            "hook_execution_id": request.started.execution_id,
            "hook_status": request.finished.status,
            "declared_effect": request.started.declared_effect,
            "workspace_snapshot_id": snapshot.workspace_snapshot_id,
            "workspace_knowledge": snapshot.workspace_knowledge,
            "status": status,
            "mutates_verification_scope": mutates_verification_scope,
            "workspace_mutation_detected_event_id": request.workspace_mutation_event_id,
            "execution_backend": request.finished.backend,
            "execution_backend_capabilities": request.finished.backend_capabilities,
            "execution_network": request.finished.network,
            "execution_resources": request.finished.resources,
            "output_redaction_state": request.output.redaction_state,
            "artifact_refs": request
                .output
                .artifact_refs
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
        }),
    )
}

fn check_event_identity(
    session: &Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    check_event: Option<&StoredEvent>,
) -> (SessionId, EventId, u64) {
    if let Some(event) = check_event {
        return (
            event.session_id.clone(),
            event.event_id.clone(),
            event.stream_sequence,
        );
    }
    let sequence = session.entries().len() as u64 + 1;
    let event_id = stable_event_uuid(
        "sigil-check-finished-memory",
        &format!("{scope:?}:{}:{sequence}", check.check_spec_hash),
    );
    ("session:in-memory".to_owned(), event_id, sequence)
}

fn environment_fingerprint(check: &CheckSpec) -> EnvironmentFingerprint {
    stable_hash_parts(
        "env",
        env::consts::OS,
        [env::consts::ARCH, check.command.command.as_str()],
        check
            .command
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
        &check.command.args.join("\0"),
        "v1",
    )
}

#[cfg(test)]
pub(super) fn sandbox_profile_hash(requirement: SandboxProfileRequirement) -> SandboxProfileHash {
    stable_hash_parts(
        "sandbox",
        requirement.as_str(),
        std::iter::empty::<&str>(),
        "",
        "",
        "v1",
    )
}

pub(super) fn sandbox_profile_hash_for_execution(
    requirement: SandboxProfileRequirement,
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: &ExecutionNetworkReceipt,
) -> SandboxProfileHash {
    let filesystem_isolation = capability_bit("filesystem", capabilities.filesystem_isolation);
    let network_isolation = capability_bit("network", capabilities.network_isolation);
    let process_isolation = capability_bit("process", capabilities.process_isolation);
    let resource_limits = capability_bit("resource_limits", capabilities.resource_limits);
    let persistent_pty = capability_bit("persistent_pty", capabilities.persistent_pty);
    let workspace_snapshot = capability_bit("workspace_snapshot", capabilities.workspace_snapshot);
    stable_hash_parts(
        "sandbox",
        requirement.as_str(),
        [
            backend.as_str(),
            filesystem_isolation.as_str(),
            network_isolation.as_str(),
            process_isolation.as_str(),
            resource_limits.as_str(),
            persistent_pty.as_str(),
            workspace_snapshot.as_str(),
            network.policy.as_str(),
        ],
        "",
        "",
        "v3",
    )
}

fn capability_bit(name: &str, value: bool) -> String {
    format!("{name}={value}")
}

impl SandboxProfileRequirement {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ApprovalOrSandbox => "approval_or_sandbox",
            Self::Sandboxed => "sandboxed",
        }
    }
}

fn format_check_command(command: &CheckCommand) -> String {
    std::iter::once(command.command.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn truncated_lossy(bytes: &[u8]) -> String {
    const MAX_PREVIEW_BYTES: usize = 4096;
    let mut value = String::from_utf8_lossy(bytes).into_owned();
    if value.len() > MAX_PREVIEW_BYTES {
        value.truncate(MAX_PREVIEW_BYTES);
        value.push_str("\n[truncated]");
    }
    value
}

pub(super) fn truncated_captured_lossy(bytes: &[u8], capture: &ExecutionStreamCapture) -> String {
    if !capture.truncated {
        return truncated_lossy(bytes);
    }
    const HALF_PREVIEW_BYTES: usize = 2048;
    let head_len = HALF_PREVIEW_BYTES.min(bytes.len());
    let tail_len = HALF_PREVIEW_BYTES.min(bytes.len().saturating_sub(head_len));
    let mut value = String::from_utf8_lossy(&bytes[..head_len]).into_owned();
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value.push_str(&format!(
        "[truncated, omitted {} bytes]\n",
        capture.omitted_bytes
    ));
    value.push_str(&String::from_utf8_lossy(
        &bytes[bytes.len().saturating_sub(tail_len)..],
    ));
    value
}
