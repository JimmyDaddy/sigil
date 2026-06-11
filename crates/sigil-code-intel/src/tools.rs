use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, Tool, ToolAccess, ToolCategory, ToolContext,
    ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, ToolSubjectScope,
};

use crate::{
    service::{CodeIntelResponse, CodeIntelligenceService},
    workspace::{canonical_workspace_root, workspace_relative_path},
};

pub fn register_code_intelligence_tools(
    registry: &mut ToolRegistry,
    config: &CodeIntelligenceConfig,
    workspace_root: PathBuf,
) -> Option<CodeIntelligenceService> {
    if !config.enabled || config.startup == CodeIntelStartup::Off {
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

impl CodeDiagnosticsTool {
    fn path_subject(&self, requested: &str) -> Result<ToolSubject> {
        path_subject(&self.service, requested)
    }
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
