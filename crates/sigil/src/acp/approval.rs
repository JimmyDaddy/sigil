use agent_client_protocol::{Client, ConnectionTo, schema::v1 as acp};
use anyhow::{Result, anyhow};
use sigil_kernel::{ApprovalHandler, ToolApproval, ToolApprovalContext, ToolCall, ToolSpec};
use tokio::sync::watch;

pub(super) struct PermissionHandler {
    pub(super) client: ConnectionTo<Client>,
    pub(super) session_id: String,
    pub(super) cancelled: watch::Receiver<bool>,
}

impl PermissionHandler {
    fn ask(
        &mut self,
        call: &ToolCall,
        context: Option<&ToolApprovalContext>,
    ) -> Result<ToolApproval> {
        if *self.cancelled.borrow() {
            return Ok(cancelled());
        }
        let mut request = acp::RequestPermissionRequest::new(
            self.session_id.clone(),
            acp::ToolCallUpdate::new(
                call.id.clone(),
                acp::ToolCallUpdateFields::new()
                    .title(call.name.clone())
                    .status(acp::ToolCallStatus::Pending)
                    .raw_input(serde_json::from_str::<serde_json::Value>(&call.args_json).ok()),
            ),
            vec![
                acp::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "deny-once",
                    "Deny",
                    acp::PermissionOptionKind::RejectOnce,
                ),
            ],
        );
        if let Some(context) = context {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "sigil/approvalIdentity".to_owned(),
                serde_json::to_value(&context.identity)?,
            );
            request = request.meta(meta);
        }
        // The shared run owns this blocking worker and already drives its async loop with
        // Tokio. Poll only this permission future here; nesting Handle::block_on would panic.
        let response = futures::executor::block_on(async {
            tokio::select! {
                biased;
                _ = wait_cancelled(&mut self.cancelled) => Ok(None),
                response = self.client.send_request(request).block_task() => {
                    response.map(Some).map_err(|error| anyhow!(error.to_string()))
                }
            }
        })?;
        if *self.cancelled.borrow() {
            return Ok(cancelled());
        }
        Ok(match response.map(|response| response.outcome) {
            Some(acp::RequestPermissionOutcome::Selected(selected))
                if selected.option_id.0.as_ref() == "allow-once" =>
            {
                ToolApproval::Approve
            }
            Some(acp::RequestPermissionOutcome::Selected(selected))
                if selected.option_id.0.as_ref() == "deny-once" =>
            {
                ToolApproval::Deny {
                    reason: "Denied in the ACP client".to_owned(),
                }
            }
            Some(acp::RequestPermissionOutcome::Selected(_)) => ToolApproval::Deny {
                reason: "ACP client returned an unoffered permission option".to_owned(),
            },
            _ => cancelled(),
        })
    }
}

fn cancelled() -> ToolApproval {
    ToolApproval::Cancelled {
        reason: "ACP permission request cancelled".to_owned(),
    }
}

pub(super) async fn wait_cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

impl ApprovalHandler for PermissionHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        self.ask(call, None)
    }

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        _spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        self.ask(call, Some(context))
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}
