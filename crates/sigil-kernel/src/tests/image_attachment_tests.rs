use anyhow::Result;

use super::*;
use crate::{CompletionRequest, ModelMessage};

fn tiny_attachment(id: &str) -> Result<ImageAttachment> {
    ImageAttachment::from_bytes(id, ImageMimeType::Png, 1, 1, vec![1, 2, 3])
}

fn request_with(message: ModelMessage) -> CompletionRequest {
    CompletionRequest {
        provider_name: "test".to_owned(),
        model_name: "test-model".to_owned(),
        messages: vec![message],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

struct FixedResolver(Vec<u8>);

impl ImageAttachmentResolver for FixedResolver {
    fn resolve(&self, _attachment: &ImageAttachment) -> Result<Vec<u8>> {
        Ok(self.0.clone())
    }
}

#[test]
fn attachment_serialization_keeps_metadata_and_omits_bytes() -> Result<()> {
    let attachment = tiny_attachment("image-1")?;
    let value = serde_json::to_value(&attachment)?;

    assert_eq!(value["mime_type"], "png");
    assert_eq!(value["byte_len"], 3);
    assert!(value.get("resolved_bytes").is_none());
    assert!(!serde_json::to_string(&value)?.contains("AQID"));
    Ok(())
}

#[test]
fn attachment_metadata_and_resolved_bytes_must_match() -> Result<()> {
    let mut attachment = tiny_attachment("image-1")?;
    let error = attachment
        .set_resolved_bytes(vec![3, 2, 1])
        .expect_err("tampered bytes must fail");
    assert!(error.to_string().contains("hash"));
    assert_eq!(attachment.resolved_bytes()?, &[1, 2, 3]);
    Ok(())
}

#[test]
fn only_user_messages_may_carry_images() -> Result<()> {
    let mut message = ModelMessage::assistant(Some("no".to_owned()), Vec::new());
    message.image_attachments.push(tiny_attachment("image-1")?);
    let error =
        validate_message_image_attachments(&message).expect_err("assistant image must fail closed");
    assert!(error.to_string().contains("only user"));
    Ok(())
}

#[test]
fn compaction_strips_blocks_but_preserves_placeholder() -> Result<()> {
    let attachment = tiny_attachment("image-1")?;
    let placeholder = render_image_attachment_placeholders(std::slice::from_ref(&attachment));
    let mut message = ModelMessage::user(format!("inspect\n\n{placeholder}"));
    message.image_attachments.push(attachment);
    let mut request = request_with(message);

    strip_request_image_attachments_for_compaction(&mut request);

    assert!(request.messages[0].image_attachments.is_empty());
    assert!(
        request.messages[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("[Image attachment 1:"))
    );
    Ok(())
}

#[test]
fn visual_token_estimate_uses_bounded_grid() {
    assert_eq!(estimate_visual_tokens(1, 1), 1);
    assert_eq!(estimate_visual_tokens(28, 28), 1);
    assert_eq!(estimate_visual_tokens(29, 29), 4);
}

#[test]
fn unsupported_capability_rejects_images_before_dispatch() -> Result<()> {
    let mut message = ModelMessage::user("inspect");
    message.image_attachments.push(tiny_attachment("image-1")?);
    let request = request_with(message);

    let error = validate_image_input_capability(ImageInputCapability::Unsupported, &request)
        .expect_err("unsupported image input must fail closed");
    assert!(error.to_string().contains("does not support image input"));
    validate_image_input_capability(ImageInputCapability::Supported, &request)?;
    Ok(())
}

#[test]
fn durable_attachment_requires_a_matching_process_local_resolver() -> Result<()> {
    let attachment = tiny_attachment("image-1")?.without_resolved_bytes();
    let mut message = ModelMessage::user("inspect");
    message.image_attachments.push(attachment);
    let mut request = request_with(message);

    let error = resolve_request_image_attachments(&mut request, None)
        .expect_err("missing cache resolver must fail before send");
    assert!(error.to_string().contains("reattach"));

    resolve_request_image_attachments(&mut request, Some(&FixedResolver(vec![1, 2, 3])))?;
    assert_eq!(
        request.messages[0].image_attachments[0].resolved_bytes()?,
        &[1, 2, 3]
    );
    Ok(())
}
