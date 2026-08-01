use std::{collections::BTreeSet, path::Path, process::Command, time::Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunOptions, ApprovalMode, ControlEntry, PermissionPolicyChain, RunEvent, Session,
    TOOL_PERMISSION_DECISION_SCHEMA_VERSION, ToolCall, ToolErrorKind, ToolExecutionStatus,
    ToolPermissionDecisionV2Entry, ToolPermissionPlanV2, ToolPermissionPlannedV2Entry,
    ToolRegistry, ToolResult, ToolSubject, ToolSubjectAudit, durable_tool_execution_entry,
    saturating_elapsed,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

pub(super) fn changed_source_files(workspace_root: &Path) -> Result<Vec<String>> {
    ensure_git_workspace(workspace_root)?;
    let mut paths = BTreeSet::new();

    if has_head(workspace_root) {
        collect_nul_paths(
            &mut paths,
            git_output(
                workspace_root,
                &[
                    "diff",
                    "--name-only",
                    "-z",
                    "--diff-filter=ACMRT",
                    "HEAD",
                    "--",
                ],
            )?,
        );
    } else {
        collect_nul_paths(
            &mut paths,
            git_output(workspace_root, &["ls-files", "-z", "--cached"])?,
        );
    }
    collect_nul_paths(
        &mut paths,
        git_output(
            workspace_root,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?,
    );

    Ok(paths
        .into_iter()
        .filter(|path| is_supported_source_file(workspace_root, path))
        .collect())
}

pub(super) fn check_changed_files_diagnostics(
    runtime: &Runtime,
    tools: &ToolRegistry,
    session: &mut Session,
    options: &AgentRunOptions,
    max_results: usize,
    paths: Vec<String>,
) -> Result<ToolResult> {
    runtime.block_on(execute_changed_files_diagnostics(
        tools,
        session,
        options,
        max_results,
        paths,
    ))
}

async fn execute_changed_files_diagnostics(
    tools: &ToolRegistry,
    session: &mut Session,
    options: &AgentRunOptions,
    max_results: usize,
    paths: Vec<String>,
) -> Result<ToolResult> {
    let call = ToolCall {
        id: format!("tui-code-diagnostics-{}", Uuid::new_v4()),
        name: "code_diagnostics".to_owned(),
        args_json: json!({
            "paths": paths,
            "max_results": max_results,
        })
        .to_string(),
    };
    let paths = diagnostics_paths_from_call(&call)?;
    let tool_ctx =
        sigil_kernel::ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs);
    let Some(spec) = tools.spec_for(&call.name) else {
        let mut result = ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Unsupported,
            "code diagnostics tool is not registered",
        );
        attach_diagnostics_context(&mut result, &paths);
        append_execution_audit(
            session,
            &call,
            &[],
            ToolExecutionStatus::Failed,
            None,
            Some(&result),
        )?;
        return Ok(result);
    };

    let permission_plan = match tools.permission_plan(&tool_ctx, &call) {
        Ok(plan) => plan,
        Err(error) => {
            let mut result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                format!("invalid code diagnostics arguments: {error}"),
            );
            attach_diagnostics_context(&mut result, &paths);
            append_execution_audit(
                session,
                &call,
                &[],
                ToolExecutionStatus::Failed,
                None,
                Some(&result),
            )?;
            return Ok(result);
        }
    };
    let decision = PermissionPolicyChain::new_with_context(
        &options.permission_config,
        &options.permission_context,
    )
    .decide_plan(&spec, &permission_plan)?;
    append_permission_audit(
        session,
        &call,
        &permission_plan,
        &decision,
        &permission_policy_version(options)?,
    )?;
    match decision.mode {
        ApprovalMode::Allow => {}
        ApprovalMode::Ask | ApprovalMode::Deny => {
            let (kind, reason) = permission_block_reason(&call, &decision);
            let mut result = ToolResult::error(call.id.clone(), call.name.clone(), kind, reason);
            attach_diagnostics_context(&mut result, &paths);
            append_execution_audit(
                session,
                &call,
                &decision.subjects,
                ToolExecutionStatus::Failed,
                None,
                Some(&result),
            )?;
            return Ok(result);
        }
    }

    append_execution_audit(
        session,
        &call,
        &decision.subjects,
        ToolExecutionStatus::Started,
        None,
        None,
    )?;
    let started = Instant::now();
    let execution_ctx = tool_ctx
        .with_approved_subjects(decision.subjects.clone())
        .with_prepared_permission_plan(permission_plan);
    let mut result = match tools.execute(execution_ctx, call.clone()).await {
        Ok(result) => result,
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Internal,
            error.to_string(),
        ),
    };
    if result.metadata.details.get("call").is_none() {
        attach_diagnostics_context(&mut result, &paths);
    }
    let status = if result.is_error() {
        ToolExecutionStatus::Failed
    } else {
        ToolExecutionStatus::Completed
    };
    append_execution_audit(
        session,
        &call,
        &decision.subjects,
        status,
        Some(duration_ms(started)),
        Some(&result),
    )?;
    Ok(result)
}

pub(super) fn diagnostics_tool_event(result: ToolResult) -> RunEvent {
    RunEvent::ToolResult(result)
}

pub(super) fn ensure_git_workspace(workspace_root: &Path) -> Result<()> {
    let output = git_command(workspace_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .with_context(|| format!("failed to run git under {}", workspace_root.display()))?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true" {
        Ok(())
    } else {
        Err(anyhow!(
            "workspace {} is not inside a git repository",
            workspace_root.display()
        ))
    }
}

pub(super) fn has_head(workspace_root: &Path) -> bool {
    git_command(workspace_root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(super) fn git_output(workspace_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_command(workspace_root)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "failed to run git {} under {}",
                args.join(" "),
                workspace_root.display()
            )
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(anyhow!(
            "git {} failed under {}{}",
            args.join(" "),
            workspace_root.display(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ))
    }
}

pub(super) fn git_command(workspace_root: &Path) -> Command {
    let mut command = Command::new("git");
    for name in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_WORK_TREE",
    ] {
        command.env_remove(name);
    }
    command.arg("-C").arg(workspace_root);
    command
}

pub(super) fn collect_nul_paths(paths: &mut BTreeSet<String>, output: Vec<u8>) {
    for raw in output.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = String::from_utf8_lossy(raw).trim().to_owned();
        if !path.is_empty() {
            paths.insert(path);
        }
    }
}

pub(super) fn is_supported_source_file(workspace_root: &Path, relative_path: &str) -> bool {
    let path = workspace_root.join(relative_path);
    if !path.is_file() {
        return false;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go")
    )
}

pub(super) fn diagnostics_paths_from_call(call: &ToolCall) -> Result<Vec<String>> {
    let args: Value = serde_json::from_str(&call.args_json)
        .with_context(|| format!("invalid tool args for {}", call.name))?;
    let paths = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing diagnostics paths"))?;
    Ok(paths
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect())
}

pub(super) fn permission_block_reason(
    call: &ToolCall,
    decision: &sigil_kernel::PermissionDecision,
) -> (ToolErrorKind, String) {
    let subject_label = if decision.subjects.is_empty() {
        call.name.clone()
    } else {
        decision
            .subjects
            .iter()
            .map(|subject| subject.normalized.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    if decision.external_directory_required {
        (
            ToolErrorKind::ExternalDirectoryRequired,
            format!(
                "external directory access requires permission.external_directory.enabled for {subject_label}. For scratch files, use $SIGIL_SCRATCH_DIR from bash or terminal_start."
            ),
        )
    } else if decision.mode == ApprovalMode::Ask {
        (
            ToolErrorKind::ApprovalRequired,
            "check changes requires approval by the current permission policy".to_owned(),
        )
    } else {
        (
            ToolErrorKind::PermissionDenied,
            format!("denied by permission policy for {subject_label}"),
        )
    }
}

fn permission_policy_version(options: &AgentRunOptions) -> Result<String> {
    let encoded = serde_json::to_vec(&options.permission_config)
        .context("failed to encode the diagnostics permission policy")?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

fn append_permission_audit(
    session: &mut Session,
    call: &ToolCall,
    plan: &ToolPermissionPlanV2,
    decision: &sigil_kernel::PermissionDecision,
    policy_version: &str,
) -> Result<()> {
    session.append_control(ControlEntry::ToolPermissionPlannedV2(Box::new(
        ToolPermissionPlannedV2Entry::from_plan(&call.id, plan)?,
    )))?;
    session.append_control(ControlEntry::ToolPermissionDecisionV2(Box::new(
        ToolPermissionDecisionV2Entry {
            schema_version: TOOL_PERMISSION_DECISION_SCHEMA_VERSION,
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            plan_hash: plan.plan_hash.clone(),
            policy_version: policy_version.to_owned(),
            policy_decision: decision.mode,
            access: decision.access,
            network_effect: decision.network_effect,
            local_policy_decision: decision.local_policy_decision,
            network_policy_decision: decision.network_policy_decision,
            source_policy_decision: decision.source_policy_decision,
            subjects: audit_subjects(&decision.subjects),
            operation: decision.operation,
            risk: decision.risk,
            subject_zones: decision.subject_zones.clone(),
            confirmation: decision.confirmation.clone(),
            snapshot_required: decision.snapshot_required,
            command_permission_matches: decision.command_permission_matches.clone(),
            external_directory_required: decision.external_directory_required,
            decision_reasons: decision.reasons.clone(),
            allow_source: None,
            grant_id: None,
            prepared_digest: None,
        },
    )))
}

fn append_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> Result<()> {
    let execution = durable_tool_execution_entry(call, subjects, status, duration_ms, result)?;
    session.append_control(ControlEntry::ToolExecution(Box::new(execution)))
}

fn audit_subjects(subjects: &[ToolSubject]) -> Vec<ToolSubjectAudit> {
    subjects.iter().map(ToolSubjectAudit::from).collect()
}

pub(super) fn attach_diagnostics_context(result: &mut ToolResult, paths: &[String]) {
    let sample_paths = paths
        .iter()
        .take(24)
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    let context = json!({
        "summary": "paths=diagnostics",
        "paths": sample_paths,
        "path_count": paths.len(),
    });
    match &mut result.metadata.details {
        Value::Object(details) => {
            details.insert("call".to_owned(), context);
        }
        Value::Null => {
            result.metadata.details = json!({ "call": context });
        }
        existing => {
            let previous = std::mem::replace(existing, Value::Null);
            result.metadata.details = json!({
                "call": context,
                "tool": previous,
            });
        }
    }
}

pub(super) fn duration_ms(started_at: Instant) -> u64 {
    saturating_elapsed(started_at)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
