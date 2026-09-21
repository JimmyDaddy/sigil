use anyhow::Result;
use sigil_kernel::{AgentProfileId, AgentThreadId};

use super::{hash_text, short_digest};

pub fn chat_agent_thread_id_for_call(
    call_id: &str,
    profile_id: &AgentProfileId,
) -> Result<AgentThreadId> {
    let hash = hash_text(&format!("chat:{}:{}", call_id, profile_id.as_str()));
    AgentThreadId::new(format!("agent_chat_{}", short_digest(&hash)))
}
