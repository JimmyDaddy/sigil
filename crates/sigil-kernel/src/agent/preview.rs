use anyhow::Result;

use crate::{
    event::{EventHandler, RunEvent},
    permission::{ApprovalMode, PermissionDecision},
    provider::ToolCall,
    session::{ControlEntry, Session, ToolApprovalAuditAction},
    tool::{
        ToolContext, ToolDiffBudget, ToolPreview, ToolPreviewCapability, ToolPreviewSnapshot,
        ToolRegistry, ToolSpec,
    },
};

use super::tool_audit::{
    append_tool_approval_audit, external_directory_preview, has_external_subject, stable_json_hash,
};

#[derive(Debug, Clone)]
pub(super) struct ToolPreviewCapture {
    pub(super) preview: Option<ToolPreview>,
    pub(super) preview_hash: Option<String>,
}

pub(super) async fn capture_tool_preview_for_decision<H>(
    session: &mut Session,
    handler: &mut H,
    tools: &ToolRegistry,
    tool_ctx: ToolContext,
    call: &ToolCall,
    spec: &ToolSpec,
    decision: &PermissionDecision,
) -> Result<ToolPreviewCapture>
where
    H: EventHandler + Send,
{
    let should_capture = matches!(decision.mode, ApprovalMode::Ask)
        || matches!(spec.preview, ToolPreviewCapability::Required);
    if !should_capture {
        return Ok(ToolPreviewCapture {
            preview: None,
            preview_hash: None,
        });
    }

    let mut preview_error = None;
    let preview = if has_external_subject(&decision.subjects) {
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

    let preview_hash = preview.as_ref().map(stable_json_hash).transpose()?;
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
    })
}
