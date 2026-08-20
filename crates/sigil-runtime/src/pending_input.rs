use anyhow::Result;
use sigil_kernel::{
    ControlEntry, ConversationInputStatus, ConversationInputStatusEntry, ModelMessage,
    PendingConversationInputProvider, PromotedConversationInput, Session,
};

use crate::current_unix_time_ms;

/// Safe-point follow-up source shared by every interactive surface: durably promotes the
/// queue's next main-thread item into the running conversation session at a turn boundary,
/// without interrupting the run.
#[derive(Clone, Default)]
pub struct DurableQueuePendingInputProvider {
    context_resolver: Option<crate::RequestContextResolver>,
}

impl DurableQueuePendingInputProvider {
    #[must_use]
    pub fn new(context_resolver: crate::RequestContextResolver) -> Self {
        Self {
            context_resolver: Some(context_resolver),
        }
    }
}

#[async_trait::async_trait]
impl PendingConversationInputProvider for DurableQueuePendingInputProvider {
    async fn promote_next_pending_input(
        &self,
        session: &mut Session,
        logical_run_id: &str,
    ) -> Result<Option<PromotedConversationInput>> {
        let snapshot = match session.active_projection_snapshot() {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) | Err(_) => return Ok(None),
        };
        let queue = snapshot.conversation_queue();
        let Some(queue_id) = queue.queue.next_dispatchable.clone() else {
            return Ok(None);
        };
        let Some(item) = queue
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id == queue_id)
        else {
            return Ok(None);
        };
        let prompt = item.queued.prompt.clone();
        // Deliver first so a crash cannot re-dispatch the same item, then append the durable
        // user message the kernel turns into the next request.
        session.append_control(ControlEntry::ConversationInputStatusChanged(
            ConversationInputStatusEntry {
                queue_id,
                status: ConversationInputStatus::Delivered,
                reason: Some(format!("injected at safe point into run {logical_run_id}")),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        ))?;
        let mut message = ModelMessage::user(prompt.clone());
        message.id = uuid::Uuid::new_v4().to_string();
        session.append_user_message(message)?;
        let runtime_context = match self.context_resolver.as_ref() {
            Some(resolver) => resolver.resolve(&prompt).await.unwrap_or_default(),
            None => Default::default(),
        };
        Ok(Some(PromotedConversationInput {
            prompt,
            runtime_context,
        }))
    }
}
