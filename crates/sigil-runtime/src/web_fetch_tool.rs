use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    ApprovalMode, EgressDisclosurePresenter, NetworkEffect, NetworkPolicy, RootConfig, Tool,
    ToolAccess, ToolCategory, ToolContext, ToolEgressAudit, ToolErrorKind, ToolOperation,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
    ToolSubjectKind, ToolSubjectScope, WebBudgetReservationKind, WebBudgetReservationRequest,
};
use sigil_tools_builtin::{WebFetchFormat, WebFetchLimits, WebFetchTransport};
use url::Url;

use crate::{
    EgressOrderingCoordinator, SystemWebDestinationResolver, WebDestinationGuard,
    WebFetchExecutionError, WebFetchExecutionOutcome, WebFetchExecutionRequest, WebFetchExecutor,
};

pub(crate) fn register_web_fetch_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    presenter: Arc<dyn EgressDisclosurePresenter>,
) {
    if root_config.web.enabled {
        registry.register(Arc::new(WebFetchTool {
            root_config: root_config.clone(),
            presenter,
        }));
    }
}

struct WebFetchTool {
    root_config: RootConfig,
    presenter: Arc<dyn EgressDisclosurePresenter>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "webfetch".to_owned(),
            description: "Fetch one exact HTTP(S) URL previously observed in this session by source_id. Use this only when the user explicitly asks to read/open a page or a specific fact is missing from existing search snippets; do not fan out across search results by default. The response is external/untrusted and subject to SSRF, redirect, decode, and task-tree budget limits.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "source_id": {
                        "type": "string",
                        "description": "A session-local source_id from the current user message or a prior web result."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "text"],
                        "default": "markdown"
                    },
                    "max_content_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional stricter model-visible response byte limit."
                    }
                },
                "required": ["source_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Read),
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_operation(&self, _ctx: &ToolContext, _args: &Value) -> Result<ToolOperation> {
        Ok(ToolOperation::NetworkRequest)
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let capability = resolve_capability(ctx, args)?;
        Ok(vec![ToolSubject {
            kind: ToolSubjectKind::NetworkEndpoint,
            original: capability.safe_display_url().to_owned(),
            normalized: capability.safe_display_url().to_owned(),
            canonical_path: None,
            scope: ToolSubjectScope::External,
        }])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(None)
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        Ok(Some(ToolEgressAudit {
            destination: "capability-backed-url".to_owned(),
            operation: "web/fetch".to_owned(),
            payload: json!({
                "source_id": args.get("source_id").and_then(Value::as_str),
                "format": args.get("format").and_then(Value::as_str).unwrap_or("markdown"),
                "max_content_bytes": args.get("max_content_bytes").and_then(Value::as_u64),
            }),
            redacted: true,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        if ctx.network_policy() == NetworkPolicy::Deny
            || (ctx.network_policy() == NetworkPolicy::Ask && !ctx.explicit_network_approval())
        {
            return Ok(ToolResult::error(
                call_id,
                "webfetch",
                ToolErrorKind::PermissionDenied,
                "webfetch requires current network authorization",
            ));
        }
        let capability = match resolve_capability(&ctx, &args) {
            Ok(capability) => capability,
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    "webfetch",
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                ));
            }
        };
        let exact_url = Url::parse(capability.raw_canonical_url().expose_secret())?;
        if exact_url.scheme() == "http" && !self.root_config.web.allow_http {
            return Ok(ToolResult::error(
                call_id,
                "webfetch",
                ToolErrorKind::PermissionDenied,
                "plaintext HTTP is disabled by web.allow_http",
            ));
        }
        if let Err(error) = crate::remote_mcp::enforce_allowed_domain(&self.root_config, &exact_url)
        {
            return Ok(ToolResult::error(
                call_id,
                "webfetch",
                ToolErrorKind::PermissionDenied,
                error.to_string(),
            ));
        }
        let format = match args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("markdown")
        {
            "markdown" => WebFetchFormat::Markdown,
            "text" => WebFetchFormat::PlainText,
            _ => {
                return Ok(ToolResult::error(
                    call_id,
                    "webfetch",
                    ToolErrorKind::InvalidInput,
                    "webfetch format must be markdown or text",
                ));
            }
        };
        let requested_model_bytes = args
            .get("max_content_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(self.root_config.web.max_model_content_bytes)
            .min(self.root_config.web.max_model_content_bytes);
        let limits = WebFetchLimits {
            max_wire_bytes: usize::try_from(self.root_config.web.max_wire_response_bytes)
                .unwrap_or(usize::MAX),
            max_decoded_bytes: usize::try_from(self.root_config.web.max_decoded_response_bytes)
                .unwrap_or(usize::MAX),
            max_model_bytes: usize::try_from(requested_model_bytes).unwrap_or(usize::MAX),
        };
        let recorder = ctx
            .egress_audit_recorder()
            .ok_or_else(|| anyhow!("webfetch requires a durable session recorder"))?;
        let registrar = ctx
            .user_url_capability_registrar()
            .ok_or_else(|| anyhow!("webfetch requires a session URL capability store"))?;
        let session_scope_id = ctx
            .session_scope_id()
            .ok_or_else(|| anyhow!("webfetch requires a session scope"))?
            .to_owned();
        let budget = ctx
            .web_task_tree_budget()
            .ok_or_else(|| anyhow!("webfetch requires a root-owned task-tree budget"))?;
        let unique = uuid::Uuid::new_v4();
        let route_fingerprint = sigil_kernel::sha256_hex(
            format!(
                "webfetch\0{}\0{}\0{:?}",
                capability.safe_display_url(),
                self.root_config.web.proxy_mode as u8,
                format
            )
            .as_bytes(),
        );
        let reservation = match budget.reserve(WebBudgetReservationRequest {
            correlation_id: format!("webfetch-{call_id}-{unique}"),
            attempt_id: format!("webfetch-attempt-{unique}"),
            route_lease_id: format!("webfetch-lease-{unique}"),
            route_fingerprint: route_fingerprint.clone(),
            kind: WebBudgetReservationKind::FetchCall,
        }) {
            Ok(reservation) => reservation,
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    "webfetch",
                    ToolErrorKind::Network,
                    error.to_string(),
                ));
            }
        };
        let destination_guard = WebDestinationGuard::new(
            SystemWebDestinationResolver,
            crate::remote_mcp::destination_policy(&self.root_config)?,
            crate::remote_mcp::proxy_environment(&self.root_config),
        );
        let executor = WebFetchExecutor::new(
            registrar,
            EgressOrderingCoordinator::new(recorder, Some(Arc::clone(&self.presenter))),
            destination_guard,
            WebFetchTransport,
        );
        let cancellation = ctx.cancellation_handle();
        let outcome = executor
            .execute(
                WebFetchExecutionRequest {
                    session_scope_id,
                    source_id: capability.source_id().to_owned(),
                    root_run_id: budget.root_run_id().to_owned(),
                    authorization_id: format!("webfetch-authorization-{unique}"),
                    disclosure_id: format!("webfetch-disclosure-{unique}"),
                    attempt_id: format!("webfetch-attempt-{unique}"),
                    route_fingerprint: route_fingerprint.clone(),
                    profile_config_proxy_fingerprint: sigil_kernel::sha256_hex(
                        format!("webfetch-profile\0{route_fingerprint}").as_bytes(),
                    ),
                    surface: "tui_or_cli".to_owned(),
                    display_name: "Web fetch".to_owned(),
                    output_durable_entry_id: call_id.clone(),
                    originating_call_id: call_id.clone(),
                    retrieved_at: crate::web_search_tool::current_rfc3339(),
                    limits,
                    format,
                },
                reservation,
                &move || {
                    cancellation
                        .as_ref()
                        .is_none_or(|handle| !handle.is_cancel_requested())
                },
            )
            .await;
        match outcome {
            Ok(WebFetchExecutionOutcome::Fetched {
                response,
                source,
                url_registration,
            }) => {
                let source = *source;
                let metadata = ToolResultMeta {
                    bytes: Some(response.model_bytes as u64),
                    truncated: response.truncated,
                    returned_bytes: Some(response.model_bytes as u64),
                    details: json!({
                        "provenance": "external_untrusted",
                        "evidence_level": "fetched_page",
                        "status": response.status,
                        "title": response.title,
                        "source": source.clone(),
                    }),
                    ..ToolResultMeta::default()
                };
                Ok(ToolResult::ok(call_id, "webfetch", response.body, metadata)
                    .with_url_capability_registrations(vec![url_registration])
                    .with_external_sources(vec![source]))
            }
            Ok(WebFetchExecutionOutcome::RedirectRequiresCapability {
                safe_display_url,
                url_registration,
            }) => Ok(ToolResult::ok(
                call_id,
                "webfetch",
                format!(
                    "Redirect target requires a new webfetch call using source_id {} ({safe_display_url}).",
                    url_registration.source_id
                ),
                ToolResultMeta {
                    details: json!({
                        "redirect_requires_capability": true,
                        "source_id": url_registration.source_id,
                        "safe_display_url": safe_display_url,
                    }),
                    ..ToolResultMeta::default()
                },
            )
            .with_url_capability_registrations(vec![url_registration])),
            Err(error) => Ok(webfetch_error(call_id, error)),
        }
    }
}

fn resolve_capability(
    ctx: &ToolContext,
    args: &Value,
) -> Result<sigil_kernel::ResolvedUserUrlCapability> {
    let source_id = args
        .get("source_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("webfetch source_id must be a non-empty string"))?;
    let session_scope_id = ctx
        .session_scope_id()
        .ok_or_else(|| anyhow!("webfetch requires an active session scope"))?;
    ctx.user_url_capability_registrar()
        .ok_or_else(|| anyhow!("webfetch requires a session URL capability store"))?
        .resolve(session_scope_id, source_id)
        .map_err(|error| anyhow!("webfetch URL capability lookup failed: {}", error.code()))
}

fn webfetch_error(call_id: String, error: WebFetchExecutionError) -> ToolResult {
    let (kind, code) = match &error {
        WebFetchExecutionError::Capability(error) => (ToolErrorKind::InvalidInput, error.code()),
        WebFetchExecutionError::InvalidCapabilityUrl
        | WebFetchExecutionError::RedirectLimitExceeded
        | WebFetchExecutionError::InvalidRedirect => (ToolErrorKind::InvalidInput, "invalid_url"),
        WebFetchExecutionError::Destination(_) => {
            (ToolErrorKind::PermissionDenied, "destination_denied")
        }
        WebFetchExecutionError::Ordering(_) => (ToolErrorKind::Network, "egress_ordering_failed"),
        WebFetchExecutionError::Transport(_) => (ToolErrorKind::Network, "transport_failed"),
        WebFetchExecutionError::Provenance(_) => (ToolErrorKind::Internal, "provenance_failed"),
    };
    ToolResult::error(call_id, "webfetch", kind, error.to_string())
        .with_error_details(false, json!({ "code": code }))
}

#[cfg(test)]
#[path = "tests/web_fetch_tool_tests.rs"]
mod tests;
