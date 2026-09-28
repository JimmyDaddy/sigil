//! Explicitly selected, reviewed plugin hooks on the ordinary tool permission boundary.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, DeclaredToolPermissionFacts, ExtensionProcessNetworkAdmission, NetworkEffect,
    SecretRedactor, Tool, ToolAccess, ToolCategory, ToolContext, ToolErrorKind, ToolOperation,
    ToolPermissionPlanDraft, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject,
};

use crate::{
    mcp_registry::McpPluginTrustSource,
    plugins::{
        ManagedPluginHookExecutionPortV1, PluginHookExecutionRequest, PluginHookExecutionRunner,
        PluginHookPreSpawnCheck, PluginHookRegistration, discover_workspace_plugins,
    },
};

/// Registers only currently reviewed hook commands. This operation does not execute hooks.
/// A failing optional plugin is reported by discovery and never becomes a run admission gate.
pub async fn register_plugin_workflow_tools(
    registry: &mut ToolRegistry,
    workspace: &Path,
    source: Arc<dyn McpPluginTrustSource>,
    executor: Arc<dyn ManagedPluginHookExecutionPortV1>,
    redactor: SecretRedactor,
) -> Result<Vec<String>> {
    let workspace_for_scan = workspace.to_path_buf();
    let source_for_scan = Arc::clone(&source);
    let report = tokio::task::spawn_blocking(move || {
        let trust = source_for_scan.current_plugin_trust()?;
        discover_workspace_plugins(&workspace_for_scan, &trust)
    })
    .await??;
    let warnings = report
        .warnings
        .iter()
        .map(|warning| format!("optional plugin discovery: {}", warning.kind.code()))
        .collect();
    for registration in report.registrations.hooks {
        let guard = Arc::new(CurrentPluginHook {
            registration,
            workspace: workspace.to_path_buf(),
            source: Arc::clone(&source),
        });
        let name = provider_hook_name(&guard.registration);
        // The opaque registration identity is an approval subject, not a fresh grant. A rebuild
        // does not let a previously approved hook inherit a new registration's execution rights.
        let subject = ToolSubject::command(
            format!(
                "plugin hook {}/{}",
                guard.registration.plugin_id,
                guard.registration.hook.stable_id()
            ),
            format!("plugin-hook-registration:{}", uuid::Uuid::new_v4()),
        );
        registry.register(Arc::new(PluginHookTool {
            name,
            subject,
            guard,
            executor: Arc::clone(&executor),
            redactor: redactor.clone(),
        }));
    }
    Ok(warnings)
}

struct CurrentPluginHook {
    registration: PluginHookRegistration,
    workspace: PathBuf,
    source: Arc<dyn McpPluginTrustSource>,
}

impl std::fmt::Debug for CurrentPluginHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentPluginHook")
            .field("plugin_id", &self.registration.plugin_id)
            .field("hook_id", &self.registration.hook.stable_id())
            .finish_non_exhaustive()
    }
}

impl PluginHookPreSpawnCheck for CurrentPluginHook {
    fn validate_current(&self) -> Result<()> {
        // The existing bounded manifest discovery rechecks canonical containment, manifest hash,
        // complete capability digest and PluginTrustEntry::matches_snapshot. Compare the exact
        // command registration as well; never infer intent from its event name or description.
        let trust = self.source.current_plugin_trust()?;
        let current = discover_workspace_plugins(&self.workspace, &trust)?;
        ensure!(
            current
                .registrations
                .hooks
                .iter()
                .any(|registration| registration == &self.registration),
            "plugin hook manifest or current trust changed; review this plugin again"
        );
        Ok(())
    }
}

struct PluginHookTool {
    name: String,
    subject: ToolSubject,
    guard: Arc<CurrentPluginHook>,
    executor: Arc<dyn ManagedPluginHookExecutionPortV1>,
    redactor: SecretRedactor,
}

#[async_trait]
impl Tool for PluginHookTool {
    fn spec(&self) -> ToolSpec {
        let registration = &self.guard.registration;
        ToolSpec {
            name: self.name.clone(),
            description: format!(
                "Run the reviewed plugin {} hook {} ({:?}) when this workflow is explicitly selected. It executes the fixed manifest command with no model-provided arguments. Normal process/network approval and current plugin trust apply. The result is command evidence, not automatic verification authority.",
                registration.plugin_id,
                registration.hook.stable_id(),
                registration.hook.kind
            ),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
            category: ToolCategory::Custom,
            access: ToolAccess::Execute,
            network_effect: Some(NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        }
    }

    fn capabilities(&self) -> std::collections::BTreeSet<sigil_kernel::ToolCapability> {
        std::collections::BTreeSet::from([sigil_kernel::ToolCapability::ProcessExecute])
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        validate_empty_args(args)?;
        sigil_kernel::declared_tool_permission_plan(
            &self.spec(),
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Execute,
                operation: ToolOperation::ExecuteUnknownCommand,
                network_effect: Some(NetworkEffect::Unknown),
                subjects: vec![self.subject.clone()],
                tool_default_mode: Some(self.guard.registration.hook.approval),
                managed_file_access: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        validate_empty_args(&args)?;
        let actual_workspace = ctx.workspace_root.clone();
        let registered_workspace = self.guard.workspace.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            ensure!(
                actual_workspace.canonicalize()? == registered_workspace.canonicalize()?,
                "plugin hook workspace changed after registration"
            );
            Ok(())
        })
        .await??;
        let mut request = PluginHookExecutionRequest::new(
            self.guard.registration.clone(),
            ctx.workspace_root.clone(),
        );
        request.mutation_recorder = ctx.mutation_recorder.clone();
        request.cancellation = ctx.cancellation_handle();
        request.redactor = self.redactor.clone();
        request.pre_spawn_check = Some(self.guard.clone());
        let runner = PluginHookExecutionRunner::new(Arc::clone(&self.executor))
            .with_network_admission(ExtensionProcessNetworkAdmission::new(
                ctx.network_policy(),
                ctx.explicit_network_approval(),
            ));
        let outcome = runner
            .execute_after_tool_approval(request, &ctx, &self.subject)
            .await?;
        let content = serde_json::to_string(&outcome.output)?;
        let succeeded =
            outcome.finished.status == sigil_kernel::PluginHookExecutionStatus::Succeeded;
        let mut result = if succeeded {
            ToolResult::ok(&call_id, &self.name, content, ToolResultMeta::default())
        } else {
            ToolResult::error(
                &call_id,
                &self.name,
                if outcome.finished.timed_out {
                    ToolErrorKind::Timeout
                } else {
                    ToolErrorKind::ExitStatus
                },
                content,
            )
        };
        result.metadata.exit_code = outcome.finished.exit_code;
        result.metadata.stdout_bytes = Some(outcome.finished.stdout_bytes);
        result.metadata.stderr_bytes = Some(outcome.finished.stderr_bytes);
        result.control_entries = vec![
            ControlEntry::PluginHookExecutionStarted(outcome.started),
            ControlEntry::PluginHookExecutionFinished(outcome.finished),
        ];
        Ok(result)
    }
}

fn validate_empty_args(args: &Value) -> Result<()> {
    ensure!(
        args.as_object().is_some_and(serde_json::Map::is_empty),
        "plugin hook accepts only an empty argument object; its command comes from the reviewed manifest"
    );
    Ok(())
}

fn provider_hook_name(registration: &PluginHookRegistration) -> String {
    let slug = |value: &str| {
        value
            .chars()
            .take(20)
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let hook_id = registration.hook.stable_id();
    let hash = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{hook_id}", registration.plugin_id).as_bytes())
    );
    format!(
        "plugin__{}__{}__{}",
        slug(&registration.plugin_id),
        slug(&hook_id),
        &hash[..8]
    )
}

#[cfg(test)]
#[path = "tests/plugin_workflow_tests.rs"]
mod tests;
