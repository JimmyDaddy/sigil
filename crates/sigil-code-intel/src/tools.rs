use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, PreparedToolExecution, Tool, ToolAccess,
    ToolCategory, ToolContext, ToolErrorKind, ToolMutationTracking, ToolPreparation, ToolPreview,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
    ToolSubjectScope,
};

use crate::{
    prepared_mutation::{PreparedMutation, PreparedMutationOutcome, PreparedMutationStatus},
    service::{CodeEditPlan, CodeIntelResponse, CodeIntelligenceService},
    workspace::{canonical_workspace_root, workspace_relative_path},
};

pub fn register_code_intelligence_tools(
    registry: &mut ToolRegistry,
    config: &CodeIntelligenceConfig,
    workspace_root: PathBuf,
) -> Option<CodeIntelligenceService> {
    if !config.enabled || config.server_startup == CodeIntelStartup::Off {
        return None;
    }
    let service = CodeIntelligenceService::new(workspace_root, config.clone());
    let service = Arc::new(service);
    registry.register(Arc::new(CodeSymbolsTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeWorkspaceSymbolsTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeDefinitionTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeReferencesTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeActionsTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeActionTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeRenameTool {
        service: Arc::clone(&service),
    }));
    registry.register(Arc::new(CodeDiagnosticsTool {
        service: Arc::clone(&service),
    }));
    Some((*service).clone())
}

struct CodeSymbolsTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeWorkspaceSymbolsTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeDefinitionTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeReferencesTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeActionsTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeActionTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeRenameTool {
    service: Arc<CodeIntelligenceService>,
}

struct CodeDiagnosticsTool {
    service: Arc<CodeIntelligenceService>,
}

#[async_trait]
impl Tool for CodeSymbolsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_symbols".to_owned(),
            description:
                "Inspect symbols in a workspace source file using LSP or Tree-sitter fallback."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "query": { "type": "string" },
                    "max_results": { "type": "integer" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![self.path_subject(required_string(args, "path")?)?])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?;
        let query = optional_string(&args, "query");
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self
            .service
            .document_symbols(path, query.as_deref(), max_results)
            .await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_symbols",
            "symbols",
            json!({ "path": path, "query": query }),
            response,
            format!(
                "path={path}{}",
                query
                    .as_ref()
                    .map(|q| format!(" query={q}"))
                    .unwrap_or_default()
            ),
        )
    }
}

#[async_trait]
impl Tool for CodeWorkspaceSymbolsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_workspace_symbols".to_owned(),
            description: "Search workspace symbols using LSP or Tree-sitter fallback.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer" }
                },
                "required": ["query"]
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![workspace_subject(self.service.workspace_root())?])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let query = required_string(&args, "query")?;
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self.service.workspace_symbols(query, max_results).await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_workspace_symbols",
            "workspace_symbols",
            json!({ "query": query }),
            response,
            format!("query={query}"),
        )
    }
}

#[async_trait]
impl Tool for CodeDefinitionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_definition".to_owned(),
            description:
                "Find definitions at a 1-based source position using a configured LSP server."
                    .to_owned(),
            input_schema: position_schema(),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![self.path_subject(required_string(args, "path")?)?])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?;
        let line = required_u64(&args, "line")?;
        let character = required_u64(&args, "character")?;
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self
            .service
            .definition(path, line, character, max_results)
            .await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_definition",
            "definition",
            json!({ "path": path, "line": line, "character": character }),
            response,
            format!("path={path} line={line} character={character}"),
        )
    }
}

#[async_trait]
impl Tool for CodeReferencesTool {
    fn spec(&self) -> ToolSpec {
        let mut schema = position_schema();
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert(
                "include_declaration".to_owned(),
                json!({ "type": "boolean" }),
            );
        }
        ToolSpec {
            name: "code_references".to_owned(),
            description:
                "Find references at a 1-based source position using a configured LSP server."
                    .to_owned(),
            input_schema: schema,
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![self.path_subject(required_string(args, "path")?)?])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?;
        let line = required_u64(&args, "line")?;
        let character = required_u64(&args, "character")?;
        let include_declaration = args
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self
            .service
            .references(path, line, character, include_declaration, max_results)
            .await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_references",
            "references",
            json!({
                "path": path,
                "line": line,
                "character": character,
                "include_declaration": include_declaration
            }),
            response,
            format!("path={path} line={line} character={character}"),
        )
    }
}

#[async_trait]
impl Tool for CodeActionsTool {
    fn spec(&self) -> ToolSpec {
        let mut schema = position_schema();
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert(
                "end_line".to_owned(),
                json!({ "type": "integer", "description": "optional 1-based end line" }),
            );
            properties.insert(
                "end_character".to_owned(),
                json!({ "type": "integer", "description": "optional 0-based UTF-16 end offset" }),
            );
            properties.insert("only".to_owned(), json!({ "type": "string" }));
        }
        ToolSpec {
            name: "code_actions".to_owned(),
            description: "List LSP code actions at a source range without applying them."
                .to_owned(),
            input_schema: schema,
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![self.path_subject(required_string(args, "path")?)?])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?;
        let line = required_u64(&args, "line")?;
        let character = required_u64(&args, "character")?;
        let end_line = optional_u64(&args, "end_line");
        let end_character = optional_u64(&args, "end_character");
        let only = optional_string(&args, "only");
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self
            .service
            .code_actions(
                path,
                line,
                character,
                end_line,
                end_character,
                only.as_deref(),
                max_results,
            )
            .await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_actions",
            "code_actions",
            json!({
                "path": path,
                "line": line,
                "character": character,
                "end_line": end_line,
                "end_character": end_character,
                "only": only
            }),
            response,
            format!("path={path} line={line} character={character}"),
        )
    }
}

#[async_trait]
impl Tool for CodeActionTool {
    fn spec(&self) -> ToolSpec {
        let mut schema = position_schema();
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert(
                "end_line".to_owned(),
                json!({ "type": "integer", "description": "optional 1-based end line" }),
            );
            properties.insert(
                "end_character".to_owned(),
                json!({ "type": "integer", "description": "optional 0-based UTF-16 end offset" }),
            );
            properties.insert("title".to_owned(), json!({ "type": "string" }));
            properties.insert("kind".to_owned(), json!({ "type": "string" }));
        }
        ToolSpec {
            name: "code_action".to_owned(),
            description: "Apply one LSP code action with a workspace diff approval preview."
                .to_owned(),
            input_schema: schema,
            category: ToolCategory::Custom,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::Controlled
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![workspace_subject(self.service.workspace_root())?])
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        let plan = self.code_action_plan(&args).await?;
        Ok(Some(
            materialize_prepared_mutation(ctx, plan, "Apply code action")
                .await?
                .0
                .preview("Apply code action"),
        ))
    }

    async fn prepare(
        &self,
        ctx: ToolContext,
        _call_id: String,
        args: Value,
    ) -> Result<Option<ToolPreparation>> {
        let plan = self.code_action_plan(&args).await?;
        let (prepared, subjects) =
            materialize_prepared_mutation(ctx, plan, "Apply code action").await?;
        let preview = prepared.preview("Apply code action");
        let digest = prepared.content_digest().to_owned();
        Ok(Some(ToolPreparation::new(
            preview, subjects, digest, prepared,
        )?))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(prepared_execution_required(call_id, "code_action"))
    }

    async fn execute_prepared(
        &self,
        ctx: ToolContext,
        _args: Value,
        prepared: PreparedToolExecution,
    ) -> Result<ToolResult> {
        execute_prepared_mutation(ctx, "code_action", prepared).await
    }
}

#[async_trait]
impl Tool for CodeRenameTool {
    fn spec(&self) -> ToolSpec {
        let mut schema = position_schema();
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert("new_name".to_owned(), json!({ "type": "string" }));
        }
        ToolSpec {
            name: "code_rename".to_owned(),
            description: "Rename one symbol through LSP with a workspace diff approval preview."
                .to_owned(),
            input_schema: schema,
            category: ToolCategory::Custom,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::Controlled
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![workspace_subject(self.service.workspace_root())?])
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        let plan = self.rename_plan(&args).await?;
        Ok(Some(
            materialize_prepared_mutation(ctx, plan, "Rename symbol")
                .await?
                .0
                .preview("Rename symbol"),
        ))
    }

    async fn prepare(
        &self,
        ctx: ToolContext,
        _call_id: String,
        args: Value,
    ) -> Result<Option<ToolPreparation>> {
        let plan = self.rename_plan(&args).await?;
        let (prepared, subjects) =
            materialize_prepared_mutation(ctx, plan, "Rename symbol").await?;
        let preview = prepared.preview("Rename symbol");
        let digest = prepared.content_digest().to_owned();
        Ok(Some(ToolPreparation::new(
            preview, subjects, digest, prepared,
        )?))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(prepared_execution_required(call_id, "code_rename"))
    }

    async fn execute_prepared(
        &self,
        ctx: ToolContext,
        _args: Value,
        prepared: PreparedToolExecution,
    ) -> Result<ToolResult> {
        execute_prepared_mutation(ctx, "code_rename", prepared).await
    }
}

#[async_trait]
impl Tool for CodeDiagnosticsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_diagnostics".to_owned(),
            description:
                "Read diagnostics for workspace source files using LSP or Tree-sitter fallback."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "severity": { "type": "string" },
                    "max_results": { "type": "integer" }
                },
                "required": ["paths"]
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let paths = string_array(args, "paths")?;
        paths
            .iter()
            .map(|path| self.path_subject(path))
            .collect::<Result<Vec<_>>>()
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let paths = string_array(&args, "paths")?;
        let severity = optional_string(&args, "severity");
        let max_results = optional_usize(&args, "max_results").unwrap_or(0);
        let response = self
            .service
            .diagnostics(&paths, severity.as_deref(), max_results)
            .await;
        result_from_response(
            self.service.config().max_payload_bytes,
            call_id,
            "code_diagnostics",
            "diagnostics",
            json!({ "paths": paths, "severity": severity }),
            response,
            "paths=diagnostics".to_owned(),
        )
    }
}

impl CodeSymbolsTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
}

impl CodeDefinitionTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
}

impl CodeReferencesTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
}

impl CodeActionsTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
}

impl CodeActionTool {
    async fn code_action_plan(&self, args: &Value) -> Result<CodeEditPlan> {
        let path = required_string(args, "path")?;
        let line = required_u64(args, "line")?;
        let character = required_u64(args, "character")?;
        let end_line = optional_u64(args, "end_line");
        let end_character = optional_u64(args, "end_character");
        let title = optional_string(args, "title");
        let kind = optional_string(args, "kind");
        self.service
            .code_action_edit_plan(
                path,
                line,
                character,
                end_line,
                end_character,
                title.as_deref(),
                kind.as_deref(),
            )
            .await
    }
}

impl CodeRenameTool {
    async fn rename_plan(&self, args: &Value) -> Result<CodeEditPlan> {
        let path = required_string(args, "path")?;
        let line = required_u64(args, "line")?;
        let character = required_u64(args, "character")?;
        let new_name = required_string(args, "new_name")?;
        self.service
            .rename_edit_plan(path, line, character, new_name)
            .await
    }
}

impl CodeDiagnosticsTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
}

async fn materialize_prepared_mutation(
    ctx: ToolContext,
    plan: CodeEditPlan,
    label: &'static str,
) -> Result<(PreparedMutation, Vec<ToolSubject>)> {
    let workspace_root = ctx.workspace_root;
    let recorder = ctx.mutation_recorder;
    run_blocking_io(label, move || {
        let prepared = PreparedMutation::materialize(&workspace_root, recorder.as_ref(), plan)?;
        let subjects = prepared
            .target_paths()
            .map(|path| exact_target_subject(&workspace_root, path))
            .collect::<Result<Vec<_>>>()?;
        Ok((prepared, subjects))
    })
    .await?
}

fn exact_target_subject(workspace_root: &std::path::Path, requested: &str) -> Result<ToolSubject> {
    let path = crate::workspace::resolve_workspace_file(workspace_root, requested)?;
    let root = canonical_workspace_root(workspace_root)?;
    Ok(ToolSubject::path_with_scope(
        requested.to_owned(),
        workspace_relative_path(&root, &path),
        Some(path),
        ToolSubjectScope::Workspace,
    ))
}

fn prepared_execution_required(call_id: String, tool_name: &str) -> ToolResult {
    ToolResult::error(
        call_id,
        tool_name,
        ToolErrorKind::StalePreparedMutation,
        "this tool requires an approval-bound prepared mutation",
    )
    .with_error_details(false, json!({ "reason": "prepared_mutation_required" }))
}

async fn execute_prepared_mutation(
    ctx: ToolContext,
    tool_name: &'static str,
    prepared: PreparedToolExecution,
) -> Result<ToolResult> {
    let call_id = prepared.binding().call_id.clone();
    let audit_binding = prepared.audit_binding();
    let artifact = prepared.into_artifact::<PreparedMutation>()?;
    execute_prepared_mutation_artifact(ctx, tool_name, call_id, audit_binding, artifact).await
}

async fn execute_prepared_mutation_artifact(
    ctx: ToolContext,
    tool_name: &'static str,
    call_id: String,
    audit_binding: sigil_kernel::PreparedToolAuditBinding,
    artifact: PreparedMutation,
) -> Result<ToolResult> {
    let cancellation = ctx.cancellation_handle();
    let Some(recorder) = ctx.mutation_recorder else {
        let details = json!({
            "reason": "mutation_recorder_required",
            "prepared_mutation": audit_binding,
        });
        let mut result = ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::DurabilityRequired,
            "durable mutation recorder is required for prepared code edits",
        )
        .with_error_details(false, details.clone());
        result.metadata.details = details;
        return Ok(result);
    };
    let workspace_root = ctx.workspace_root;
    let binding_for_execution = audit_binding.clone();
    let call_id_for_execution = call_id.clone();
    let outcome = run_blocking_io("execute prepared code mutation", move || {
        artifact.execute(
            &workspace_root,
            &recorder,
            &binding_for_execution,
            &call_id_for_execution,
            cancellation.as_ref(),
        )
    })
    .await??;
    prepared_mutation_result(tool_name, call_id, audit_binding, outcome)
}

#[cfg(test)]
async fn execute_prepared_mutation_for_test(
    ctx: ToolContext,
    tool_name: &'static str,
    call_id: String,
    policy_fingerprint: &str,
    artifact: PreparedMutation,
) -> Result<ToolResult> {
    let content_digest = artifact.content_digest().to_owned();
    let prepared_digest = sigil_kernel::stable_event_hash(
        format!("{call_id}:{tool_name}:{policy_fingerprint}:{content_digest}").as_bytes(),
    );
    let audit_binding = sigil_kernel::PreparedToolAuditBinding {
        schema_version: 1,
        approval_identity: format!("test-approval:{call_id}"),
        prepared_digest,
        content_digest,
        args_digest: sigil_kernel::stable_event_hash(call_id.as_bytes()),
        policy_fingerprint: policy_fingerprint.to_owned(),
    };
    execute_prepared_mutation_artifact(ctx, tool_name, call_id, audit_binding, artifact).await
}

fn prepared_mutation_result(
    tool_name: &str,
    call_id: String,
    binding: sigil_kernel::PreparedToolAuditBinding,
    outcome: PreparedMutationOutcome,
) -> Result<ToolResult> {
    let status = match outcome.status {
        PreparedMutationStatus::Applied => "applied",
        PreparedMutationStatus::RolledBack => "rolled_back",
        PreparedMutationStatus::RollbackFailed => "rollback_failed",
        PreparedMutationStatus::Failed => "failed",
        PreparedMutationStatus::Stale => "stale",
    };
    let details = json!({
        "prepared_mutation": binding,
        "prepared_mutation_result": {
            "status": status,
            "batch_id": outcome.batch_id,
            "base_workspace_revision": outcome.base_workspace_revision,
            "committed_operations": outcome.committed_operations,
            "failed_operations": outcome.failed_operations,
            "rollback_operations": outcome.rollback_operations,
            "rollback_failed_operations": outcome.rollback_failed_operations,
            "residual_files": outcome.residual_files,
            "reason": outcome.reason,
        },
        "code_intelligence": {
            "server": outcome.server,
            "capability": outcome.capability,
        }
    });
    let content = serde_json::to_string(&json!({
        "tool": tool_name,
        "status": status,
        "changed_files": outcome.changed_files,
        "applied_edits": outcome.applied_edits,
    }))?;
    let metadata = ToolResultMeta {
        duration_ms: Some(outcome.query_elapsed_ms),
        returned_entries: Some(outcome.applied_edits as u64),
        total_entries: Some(outcome.applied_edits as u64),
        changed_files: outcome.changed_files,
        details: details.clone(),
        ..ToolResultMeta::default()
    };
    if outcome.status == PreparedMutationStatus::Applied {
        return Ok(ToolResult::ok(call_id, tool_name, content, metadata));
    }
    let kind = if outcome.status == PreparedMutationStatus::Stale {
        ToolErrorKind::StalePreparedMutation
    } else {
        ToolErrorKind::Io
    };
    let mut result = ToolResult::error(
        call_id,
        tool_name,
        kind,
        format!("prepared mutation {status}"),
    )
    .with_error_details(false, details);
    result.metadata = metadata;
    Ok(result)
}

async fn run_blocking_io<T, F>(label: &'static str, job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|error| anyhow!("{label} blocking task failed: {error}"))
}

fn result_from_response<T>(
    max_payload_bytes: usize,
    call_id: String,
    tool_name: &str,
    result_key: &str,
    query: Value,
    response: Result<CodeIntelResponse<T>>,
    call_summary: String,
) -> Result<ToolResult>
where
    T: Clone + Serialize,
{
    match response {
        Ok(response) => {
            let mut results = response.results;
            let mut metadata = response.metadata.clone();
            let (content_text, results_truncated) = bounded_response_content(
                max_payload_bytes,
                tool_name,
                result_key,
                &query,
                &response.server,
                &response.capability,
                &response.server_statuses,
                &mut results,
                &mut metadata,
            )?;
            if results_truncated {
                metadata.truncated = true;
            }
            let metadata = ToolResultMeta {
                duration_ms: Some(response.metadata.elapsed_ms),
                truncated: metadata.truncated,
                returned_entries: Some(metadata.returned as u64),
                total_entries: Some(response.metadata.total as u64),
                bytes: Some(content_text.len() as u64),
                details: json!({
                    "call": { "summary": call_summary },
                    "code_intelligence": {
                        "server": response.server,
                        "capability": response.capability,
                        "returned": metadata.returned,
                        "total": response.metadata.total,
                        "truncated": metadata.truncated,
                        "external_results_filtered": response.metadata.external_results_filtered,
                        "status_line": format!("ready {}", response.server),
                        "servers": response.server_statuses
                    }
                }),
                ..ToolResultMeta::default()
            };
            Ok(ToolResult::ok(call_id, tool_name, content_text, metadata))
        }
        Err(error) => {
            let message = error.to_string();
            Ok(ToolResult::error(
                call_id,
                tool_name,
                classify_error(&message),
                message.clone(),
            )
            .with_error_details(
                false,
                json!({
                    "call": { "summary": call_summary },
                    "code_intelligence": {
                        "status_line": format!("degraded {message}")
                    }
                }),
            ))
        }
    }
}

fn bounded_response_content<T>(
    max_payload_bytes: usize,
    tool_name: &str,
    result_key: &str,
    query: &Value,
    server: &str,
    capability: &str,
    server_statuses: &[crate::service::CodeIntelServerStatus],
    results: &mut Vec<T>,
    metadata: &mut crate::service::QueryMetadata,
) -> Result<(String, bool)>
where
    T: Clone + Serialize,
{
    let byte_limit = max_payload_bytes.max(512);
    let mut truncated = false;
    loop {
        metadata.returned = results.len();
        let content = json!({
            "tool": tool_name,
            "status": "ok",
            "server": server,
            "capability": capability,
            "query": query,
            result_key: results.clone(),
            "results": results.clone(),
            "servers": server_statuses,
            "metadata": metadata
        });
        let content_text = serde_json::to_string(&content)?;
        if content_text.len() <= byte_limit || results.is_empty() {
            return Ok((content_text, truncated));
        }
        results.pop();
        metadata.truncated = true;
        truncated = true;
    }
}

fn classify_error(message: &str) -> ToolErrorKind {
    if message.contains("outside workspace") {
        ToolErrorKind::PathOutsideWorkspace
    } else if message.contains("does not exist") || message.contains("not found") {
        ToolErrorKind::NotFound
    } else if message.contains("timed out") {
        ToolErrorKind::Timeout
    } else if message.contains("does not support") || message.contains("disabled") {
        ToolErrorKind::Unsupported
    } else {
        ToolErrorKind::Protocol
    }
}

fn path_subject(service: &CodeIntelligenceService, requested: &str) -> Result<ToolSubject> {
    let path = service.resolve_file(requested)?;
    let root = canonical_workspace_root(service.workspace_root())?;
    Ok(ToolSubject::path_with_scope(
        requested.to_owned(),
        workspace_relative_path(&root, &path),
        Some(path),
        ToolSubjectScope::Workspace,
    ))
}

fn workspace_subject(workspace_root: &std::path::Path) -> Result<ToolSubject> {
    let root = canonical_workspace_root(workspace_root)?;
    Ok(ToolSubject::path_with_scope(
        ".".to_owned(),
        ".".to_owned(),
        Some(root),
        ToolSubjectScope::Workspace,
    ))
}

fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "line": { "type": "integer", "description": "1-based line number" },
            "character": { "type": "integer", "description": "0-based UTF-16 character offset" },
            "max_results": { "type": "integer" }
        },
        "required": ["path", "line", "character"]
    })
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("missing required string argument {key}"))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

fn required_u64(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing required integer argument {key}"))
}

fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn string_array(args: &Value, key: &str) -> Result<Vec<String>> {
    let values = args
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing required string array argument {key}"))?;
    let paths = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("{key} must contain only non-empty strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    if paths.is_empty() {
        return Err(anyhow!("{key} cannot be empty"));
    }
    Ok(paths)
}

#[cfg(test)]
#[path = "tests/tools_tests.rs"]
mod tests;
