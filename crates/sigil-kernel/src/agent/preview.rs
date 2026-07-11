use anyhow::Result;
use serde_json::json;

use crate::{
    event::{EventHandler, RunEvent},
    permission::{ApprovalMode, PermissionDecision},
    provider::ToolCall,
    session::{
        ControlEntry, Session, SessionLogEntry, ToolApprovalAuditAction,
        ToolApprovalSessionGrantEntry, ToolApprovalUserDecision,
    },
    tool::{
        PreparedToolCall, ToolContext, ToolDiffBudget, ToolPreview, ToolPreviewCapability,
        ToolPreviewSnapshot, ToolRegistry, ToolSpec,
    },
};

use super::{
    approval_policy::PlanApprovalAuthority,
    tool_audit::{
        append_tool_approval_audit, external_directory_preview, has_external_subject,
        stable_json_hash,
    },
};

#[derive(Debug)]
pub(super) struct ToolPreviewCapture {
    pub(super) preview: Option<ToolPreview>,
    pub(super) preview_hash: Option<String>,
    pub(super) prepared: Option<PreparedToolCall>,
}

pub(super) fn preparation_policy_fingerprint(decision: &PermissionDecision) -> Result<String> {
    stable_json_hash(&json!({
        "schema_version": 2,
        "mode": decision.mode,
        "access": decision.access,
        "network_effect": decision.network_effect,
        "local_policy_decision": decision.local_policy_decision,
        "network_policy_decision": decision.network_policy_decision,
        "source_policy_decision": decision.source_policy_decision,
        "operation": decision.operation,
        "risk": decision.risk,
        "subjects": decision.subjects,
        "subject_zones": decision.subject_zones,
        "subject_risk_overlays": decision.subject_risk_overlays,
        "external_directory_required": decision.external_directory_required,
        "confirmation": decision.confirmation,
        "snapshot_required": decision.snapshot_required,
        "command_permission_matches": decision.command_permission_matches,
    }))
    .map(|digest| format!("sha256:{digest}"))
}

pub(super) fn preparation_policy_approval_identity(policy_fingerprint: &str) -> String {
    format!("policy:{policy_fingerprint}")
}

pub(super) fn preparation_session_grant_identity(
    grant: &ToolApprovalSessionGrantEntry,
) -> Result<String> {
    stable_json_hash(&serde_json::to_value(grant)?).map(|digest| format!("session-grant:{digest}"))
}

pub(super) fn preparation_plan_approval_identity(
    authority: &PlanApprovalAuthority,
) -> Result<String> {
    let value = match authority {
        PlanApprovalAuthority::PermissionGrant(grant) => {
            json!({"kind": "plan_permission_grant", "entry": grant})
        }
        PlanApprovalAuthority::ApprovedPlan(approval) => {
            json!({"kind": "approved_plan", "entry": approval})
        }
    };
    stable_json_hash(&value).map(|digest| format!("plan:{digest}"))
}

pub(super) fn resolved_interactive_approval_identity(
    session: &Session,
    call_id: &str,
    prepared_digest: &str,
) -> Result<Option<String>> {
    let approval = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::ToolApproval(approval)) = entry else {
            return None;
        };
        (approval.call_id == call_id
            && approval.action == ToolApprovalAuditAction::Resolved
            && matches!(
                approval.user_decision,
                Some(
                    ToolApprovalUserDecision::Approved
                        | ToolApprovalUserDecision::ApprovedForSession
                )
            )
            && approval.preview_hash.as_deref() == Some(prepared_digest))
        .then_some(approval)
    });
    approval
        .map(|approval| {
            stable_json_hash(&serde_json::to_value(approval)?)
                .map(|digest| format!("interactive:{digest}"))
        })
        .transpose()
}

pub(super) fn pending_interactive_approval_identity(call_id: &str) -> String {
    format!("interactive-pending:{call_id}")
}

pub(super) async fn capture_tool_preview_for_decision<H>(
    session: &mut Session,
    handler: &mut H,
    tools: &ToolRegistry,
    tool_ctx: ToolContext,
    call: &ToolCall,
    spec: &ToolSpec,
    decision: &PermissionDecision,
    prepared: Option<PreparedToolCall>,
) -> Result<ToolPreviewCapture>
where
    H: EventHandler + Send,
{
    let should_capture = matches!(decision.mode, ApprovalMode::Ask)
        || matches!(spec.preview, ToolPreviewCapability::Required);
    if !should_capture {
        let preview_hash = prepared
            .as_ref()
            .map(|prepared| prepared.prepared_digest().to_owned());
        return Ok(ToolPreviewCapture {
            preview: None,
            preview_hash,
            prepared,
        });
    }

    let mut preview_error = None;
    let preview = if let Some(prepared) = prepared.as_ref() {
        Some(prepared.preview().clone())
    } else if has_external_subject(&decision.subjects) {
        Some(external_directory_preview(&call.name, &decision.subjects))
    } else {
        match tools.preview(tool_ctx, call.clone()).await {
            Ok(preview) => preview,
            Err(error) => {
                let error = error.to_string();
                preview_error = Some(error.clone());
                matches!(decision.mode, ApprovalMode::Ask).then(|| ToolPreview {
                    title: format!("Preview unavailable for {}", call.name),
                    summary: "The tool preview could not be generated automatically.".to_owned(),
                    body: error,
                    changed_files: Vec::new(),
                    file_diffs: Vec::new(),
                })
            }
        }
    };

    if let Some(error) = preview_error.as_ref() {
        append_tool_approval_audit(
            session,
            call,
            decision,
            ToolApprovalAuditAction::PreviewFailed,
            None,
            Some(error.clone()),
            None,
        )?;
    }

    let preview_hash = prepared
        .as_ref()
        .map(|prepared| prepared.prepared_digest().to_owned())
        .or(preview.as_ref().map(stable_json_hash).transpose()?);
    if preview_error.is_none()
        && let Some(preview) = preview.as_ref()
    {
        let control = ControlEntry::ToolPreviewCaptured(ToolPreviewSnapshot::from_preview(
            call.id.clone(),
            call.name.clone(),
            preview,
            ToolDiffBudget::default(),
            preview_hash.clone(),
        ));
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }

    Ok(ToolPreviewCapture {
        preview,
        preview_hash,
        prepared,
    })
}
