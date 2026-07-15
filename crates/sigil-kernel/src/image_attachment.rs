use std::{collections::BTreeSet, fmt, sync::Arc};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CompletionRequest, MessageRole, ModelMessage};

pub const MAX_IMAGE_ATTACHMENTS_PER_TURN: usize = 4;
pub const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_IMAGE_ATTACHMENT_BYTES_PER_TURN: u64 = 24 * 1024 * 1024;
pub const MAX_IMAGE_ATTACHMENT_DIMENSION: u32 = 8_192;
pub const MAX_IMAGE_ATTACHMENT_PIXELS: u64 = 16_000_000;
pub const MAX_IMAGE_ATTACHMENT_VISUAL_TOKENS_PER_TURN: u64 = 16_384;

/// Image formats admitted by the provider-neutral V1 attachment contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ImageMimeType {
    Png,
    Jpeg,
    Webp,
}

impl ImageMimeType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}

/// Exact-model image input support declared by one provider implementation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageInputCapability {
    #[default]
    Unsupported,
    Supported,
}

impl ImageInputCapability {
    #[must_use]
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Provider-neutral durable image metadata plus optional process-local bytes.
///
/// `resolved_bytes` is deliberately excluded from every serde surface. Durable session state,
/// request prefix evidence, exports, and compaction therefore retain only the content binding and
/// controlled cache reference.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ImageAttachment {
    pub attachment_id: String,
    pub sha256: String,
    pub mime_type: ImageMimeType,
    pub width: u32,
    pub height: u32,
    pub byte_len: u64,
    pub estimated_visual_tokens: u64,
    pub artifact_ref: String,
    #[serde(skip)]
    resolved_bytes: Option<Arc<[u8]>>,
}

impl fmt::Debug for ImageAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageAttachment")
            .field("attachment_id", &self.attachment_id)
            .field("sha256", &self.sha256)
            .field("mime_type", &self.mime_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("byte_len", &self.byte_len)
            .field("estimated_visual_tokens", &self.estimated_visual_tokens)
            .field("artifact_ref", &self.artifact_ref)
            .field(
                "resolved_bytes",
                &self
                    .resolved_bytes
                    .as_ref()
                    .map(|bytes| format!("[redacted; {} bytes]", bytes.len())),
            )
            .finish()
    }
}

impl ImageAttachment {
    /// Constructs a resolved attachment after a runtime ingress path has identified its format and
    /// dimensions. The byte hash, artifact reference, and visual-token estimate are derived here.
    pub fn from_bytes(
        attachment_id: impl Into<String>,
        mime_type: ImageMimeType,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    ) -> Result<Self> {
        let attachment_id = attachment_id.into();
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let artifact_ref = format!("{sha256}.{}", mime_type.extension());
        let attachment = Self {
            attachment_id,
            sha256,
            mime_type,
            width,
            height,
            byte_len: bytes.len() as u64,
            estimated_visual_tokens: estimate_visual_tokens(width, height),
            artifact_ref,
            resolved_bytes: Some(Arc::from(bytes)),
        };
        attachment.validate()?;
        Ok(attachment)
    }

    /// Validates durable metadata and any resolved byte binding.
    pub fn validate(&self) -> Result<()> {
        validate_attachment_id(&self.attachment_id)?;
        validate_sha256(&self.sha256)?;
        if self.width == 0 || self.height == 0 {
            bail!("image attachment dimensions must be non-zero");
        }
        if self.width > MAX_IMAGE_ATTACHMENT_DIMENSION
            || self.height > MAX_IMAGE_ATTACHMENT_DIMENSION
        {
            bail!("image attachment exceeds the maximum dimension");
        }
        let pixels = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or_else(|| anyhow!("image attachment pixel count overflowed"))?;
        if pixels > MAX_IMAGE_ATTACHMENT_PIXELS {
            bail!("image attachment exceeds the maximum pixel count");
        }
        if self.byte_len == 0 || self.byte_len > MAX_IMAGE_ATTACHMENT_BYTES {
            bail!("image attachment byte length is outside the V1 limit");
        }
        let expected_tokens = estimate_visual_tokens(self.width, self.height);
        if self.estimated_visual_tokens != expected_tokens {
            bail!("image attachment visual-token estimate does not match its dimensions");
        }
        let expected_ref = format!("{}.{}", self.sha256, self.mime_type.extension());
        if self.artifact_ref != expected_ref {
            bail!("image attachment artifact reference is not canonical");
        }
        if let Some(bytes) = &self.resolved_bytes {
            if bytes.len() as u64 != self.byte_len {
                bail!("resolved image attachment byte length does not match metadata");
            }
            let observed = format!("{:x}", Sha256::digest(bytes.as_ref()));
            if observed != self.sha256 {
                bail!("resolved image attachment hash does not match metadata");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn without_resolved_bytes(&self) -> Self {
        let mut durable = self.clone();
        durable.resolved_bytes = None;
        durable
    }

    pub fn set_resolved_bytes(&mut self, bytes: Vec<u8>) -> Result<()> {
        let previous = self.resolved_bytes.replace(Arc::from(bytes));
        if let Err(error) = self.validate() {
            self.resolved_bytes = previous;
            return Err(error);
        }
        Ok(())
    }

    #[must_use]
    pub fn has_resolved_bytes(&self) -> bool {
        self.resolved_bytes.is_some()
    }

    pub fn resolved_bytes(&self) -> Result<&[u8]> {
        self.resolved_bytes
            .as_deref()
            .ok_or_else(|| anyhow!("image attachment bytes are not resolved"))
    }
}

/// Process-local bridge from durable attachment metadata to controlled-cache bytes.
pub trait ImageAttachmentResolver: Send + Sync {
    fn resolve(&self, attachment: &ImageAttachment) -> Result<Vec<u8>>;
}

#[must_use]
pub fn estimate_visual_tokens(width: u32, height: u32) -> u64 {
    u64::from(width).div_ceil(28) * u64::from(height).div_ceil(28)
}

pub fn validate_message_image_attachments(message: &ModelMessage) -> Result<()> {
    if !message.image_attachments.is_empty() && message.role != MessageRole::User {
        bail!("only user messages may carry image attachments");
    }
    if message.image_attachments.len() > MAX_IMAGE_ATTACHMENTS_PER_TURN {
        bail!("user message exceeds the image attachment count limit");
    }
    let mut ids = BTreeSet::new();
    let mut total_bytes = 0_u64;
    let mut total_visual_tokens = 0_u64;
    for attachment in &message.image_attachments {
        attachment.validate()?;
        if !ids.insert(attachment.attachment_id.as_str()) {
            bail!("user message contains a duplicate image attachment id");
        }
        total_bytes = total_bytes
            .checked_add(attachment.byte_len)
            .ok_or_else(|| anyhow!("image attachment byte total overflowed"))?;
        total_visual_tokens = total_visual_tokens
            .checked_add(attachment.estimated_visual_tokens)
            .ok_or_else(|| anyhow!("image attachment visual-token total overflowed"))?;
    }
    if total_bytes > MAX_IMAGE_ATTACHMENT_BYTES_PER_TURN {
        bail!("user message exceeds the image attachment byte limit");
    }
    if total_visual_tokens > MAX_IMAGE_ATTACHMENT_VISUAL_TOKENS_PER_TURN {
        bail!("user message exceeds the image attachment visual-token limit");
    }
    Ok(())
}

pub fn validate_request_image_attachments(request: &CompletionRequest) -> Result<()> {
    for message in &request.messages {
        validate_message_image_attachments(message)?;
    }
    Ok(())
}

pub fn validate_image_input_capability(
    capability: ImageInputCapability,
    request: &CompletionRequest,
) -> Result<()> {
    if request
        .messages
        .iter()
        .any(|message| !message.image_attachments.is_empty())
        && !capability.is_supported()
    {
        bail!(
            "provider model {} does not support image input",
            request.model_name
        );
    }
    Ok(())
}

pub fn resolve_request_image_attachments(
    request: &mut CompletionRequest,
    resolver: Option<&dyn ImageAttachmentResolver>,
) -> Result<()> {
    for message in &mut request.messages {
        validate_message_image_attachments(message)?;
        for attachment in &mut message.image_attachments {
            if !attachment.has_resolved_bytes() {
                let resolver = resolver.ok_or_else(|| {
                    anyhow!(
                        "image attachment {} is unavailable; reattach the image or restore the controlled cache",
                        attachment.attachment_id
                    )
                })?;
                let bytes = resolver.resolve(attachment)?;
                attachment.set_resolved_bytes(bytes)?;
            }
        }
    }
    Ok(())
}

/// Removes provider image blocks while preserving the fixed textual placeholder in message
/// content. Compaction paths must call this before freezing or mapping provider-native input.
pub fn strip_request_image_attachments_for_compaction(request: &mut CompletionRequest) {
    for message in &mut request.messages {
        message.image_attachments.clear();
    }
}

#[must_use]
pub fn render_image_attachment_placeholders(attachments: &[ImageAttachment]) -> String {
    attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| {
            let short_hash = attachment.sha256.get(..12).unwrap_or(&attachment.sha256);
            format!(
                "[Image attachment {}: {}; {}x{}; {} bytes; sha256={short_hash}]",
                index + 1,
                attachment.mime_type.as_str(),
                attachment.width,
                attachment.height,
                attachment.byte_len,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_attachment_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("image attachment id is invalid");
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        bail!("image attachment SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/image_attachment_tests.rs"]
mod tests;
