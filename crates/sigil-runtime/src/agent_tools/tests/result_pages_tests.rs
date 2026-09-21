use super::*;

#[test]
fn agent_final_text_reference_must_match_character_count() -> Result<()> {
    let content = "a🙂bc";
    let message = sigil_kernel::ModelMessage::assistant_with_kind(
        Some(content.to_owned()),
        Vec::new(),
        sigil_kernel::AssistantMessageKind::FinalAnswer,
    );
    let reference = sigil_kernel::AgentFinalAnswerRef {
        session_ref: sigil_kernel::SessionRef::new_relative("child.jsonl")?,
        message_id: message.id.clone(),
        content_hash: hash_text(content),
        char_count: content.chars().count() + 1,
    };

    let error = agent_final_text_from_ref(
        &[sigil_kernel::SessionLogEntry::Assistant(message)],
        &reference,
    )
    .expect_err("a mismatched persisted character count must be rejected");

    assert!(error.to_string().contains("character count mismatch"));
    Ok(())
}
