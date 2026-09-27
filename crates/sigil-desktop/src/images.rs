//! Narrow image transport types. Encoded content is transient and never implements Debug.
use serde::{Deserialize, Serialize};

/// Maximum encoded bytes per image, matching the shared image admission contract.
pub const MAX_DESKTOP_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum attachment references accepted in one foreground input.
pub const MAX_DESKTOP_IMAGES_PER_TURN: usize = 4;
/// Maximum combined encoded bytes in one foreground input.
pub const MAX_DESKTOP_IMAGE_BYTES_PER_TURN: u64 = 24 * 1024 * 1024;

/// Server-admitted image metadata exchanged only between native shell and private HTTP client.
/// The renderer receives a narrower projection without the hash or controlled-cache reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopImageAttachment {
    /// Opaque identity issued independently for each ingestion.
    pub attachment_id: String,
    /// Hash binding the metadata to the exact encoded bytes.
    pub sha256: String,
    /// Shared wire-format image kind, such as `png`.
    pub mime_type: String,
    /// Decoded pixel width checked by shared admission.
    pub width: u32,
    /// Decoded pixel height checked by shared admission.
    pub height: u32,
    /// Encoded content length checked against the upload.
    pub byte_len: u64,
    /// Shared bounded estimate used for image context accounting.
    pub estimated_visual_tokens: u64,
    /// Controlled-cache reference; never an arbitrary renderer-provided path.
    pub artifact_ref: String,
}

/// Transient content read from an exact durable session/message/attachment binding.
/// Encoded bytes are excluded from Debug output and are not a new persistence owner.
#[derive(Deserialize)]
pub struct DesktopImageContent {
    /// Full media type, such as `image/png`, for the native preview data URL.
    pub mime_type: String,
    /// Bounded base64 representation of the cache-verified encoded bytes.
    pub data_base64: String,
}
