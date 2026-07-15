use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use image::{ImageFormat, ImageReader};
use sigil_kernel::{
    ImageAttachment, ImageAttachmentResolver, ImageMimeType, MAX_IMAGE_ATTACHMENT_BYTES,
};
use tempfile::NamedTempFile;
use url::Url;

/// Workspace-scoped content-addressed cache for encoded image attachments.
#[derive(Debug, Clone)]
pub struct ControlledImageAttachmentCache {
    root: PathBuf,
}

impl ControlledImageAttachmentCache {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Admits one no-follow regular image file, validates it, and atomically stores its encoded
    /// bytes under the derived content-addressed artifact reference.
    pub fn ingest_path(
        &self,
        attachment_id: impl Into<String>,
        source_path: &Path,
    ) -> Result<ImageAttachment> {
        let bytes = read_bounded_regular_file(source_path, MAX_IMAGE_ATTACHMENT_BYTES)
            .with_context(|| format!("failed to read image {}", source_path.display()))?;
        self.ingest_encoded_bytes(attachment_id, bytes)
    }

    /// Admits already-encoded clipboard or file bytes. The format is derived from the bytes, not
    /// from a file name or caller-provided MIME label.
    pub fn ingest_encoded_bytes(
        &self,
        attachment_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<ImageAttachment> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_ATTACHMENT_BYTES {
            bail!("image attachment byte length is outside the V1 limit");
        }
        let identified = identify_and_decode_image(&bytes)?;
        let attachment = ImageAttachment::from_bytes(
            attachment_id,
            identified.mime_type,
            identified.width,
            identified.height,
            bytes,
        )?;
        self.persist(&attachment)?;
        Ok(attachment)
    }

    fn persist(&self, attachment: &ImageAttachment) -> Result<()> {
        attachment.validate()?;
        ensure_cache_root(&self.root)?;
        let target = self.root.join(&attachment.artifact_ref);
        if target.exists() {
            self.verify_cached_attachment(attachment)?;
            return Ok(());
        }

        let mut staged = NamedTempFile::new_in(&self.root).with_context(|| {
            format!(
                "failed to create image attachment staging file in {}",
                self.root.display()
            )
        })?;
        staged
            .as_file_mut()
            .write_all(attachment.resolved_bytes()?)
            .context("failed to stage image attachment bytes")?;
        staged
            .as_file_mut()
            .sync_all()
            .context("failed to sync staged image attachment")?;
        match staged.persist_noclobber(&target) {
            Ok(file) => {
                file.sync_all()
                    .context("failed to sync persisted image attachment")?;
                sync_parent_directory(&self.root)?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_cached_attachment(attachment)?;
            }
            Err(error) => {
                return Err(error.error).with_context(|| {
                    format!("failed to persist image attachment {}", target.display())
                });
            }
        }
        self.verify_cached_attachment(attachment).map(drop)
    }

    fn verify_cached_attachment(&self, attachment: &ImageAttachment) -> Result<Vec<u8>> {
        attachment.validate()?;
        ensure_cache_root(&self.root)?;
        let path = self.root.join(&attachment.artifact_ref);
        let bytes = read_bounded_regular_file(&path, MAX_IMAGE_ATTACHMENT_BYTES)
            .with_context(|| format!("failed to resolve cached image {}", path.display()))?;
        let identified = identify_and_decode_image(&bytes)?;
        if identified.mime_type != attachment.mime_type
            || identified.width != attachment.width
            || identified.height != attachment.height
        {
            bail!("cached image format or dimensions do not match durable metadata");
        }
        let mut verified = attachment.clone();
        verified.set_resolved_bytes(bytes.clone())?;
        Ok(bytes)
    }
}

impl ImageAttachmentResolver for ControlledImageAttachmentCache {
    fn resolve(&self, attachment: &ImageAttachment) -> Result<Vec<u8>> {
        self.verify_cached_attachment(attachment)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IdentifiedImage {
    mime_type: ImageMimeType,
    width: u32,
    height: u32,
}

fn identify_and_decode_image(bytes: &[u8]) -> Result<IdentifiedImage> {
    let format = image::guess_format(bytes).context("image format could not be identified")?;
    let mime_type = match format {
        ImageFormat::Png => ImageMimeType::Png,
        ImageFormat::Jpeg => ImageMimeType::Jpeg,
        ImageFormat::WebP => ImageMimeType::Webp,
        _ => bail!("image format is outside the PNG/JPEG/WebP V1 allowlist"),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .context("failed to read image dimensions")?;
    let probe =
        ImageAttachment::from_bytes("dimension-probe", mime_type, width, height, bytes.to_vec())?;
    drop(probe);
    image::load_from_memory_with_format(bytes, format)
        .context("image payload is malformed or truncated")?;
    Ok(IdentifiedImage {
        mime_type,
        width,
        height,
    })
}

fn ensure_cache_root(root: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(root) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("image attachment cache root must be a non-symlink directory");
        }
        return Ok(());
    }
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create image attachment cache {}", root.display()))?;
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("image attachment cache root must be a non-symlink directory");
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let path_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        bail!("image attachment path must be a no-follow regular file");
    }
    if path_metadata.len() == 0 || path_metadata.len() > max_bytes {
        bail!("image attachment byte length is outside the V1 limit");
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened file {}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() == 0 || opened_metadata.len() > max_bytes
    {
        bail!("opened image attachment is not a bounded regular file");
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes {
        bail!("image attachment byte length is outside the V1 limit");
    }
    if bytes.len() as u64 != opened_metadata.len() {
        bail!("image attachment changed while it was being read");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_parent_directory(root: &Path) -> Result<()> {
    File::open(root)
        .with_context(|| format!("failed to open cache directory {}", root.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync cache directory {}", root.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_root: &Path) -> Result<()> {
    Ok(())
}

/// Recognizes a single pasted PNG/JPEG/WebP path or `file://` URL. Ordinary text, multiline paste,
/// unsupported extensions, and non-existent paths remain composer text.
#[must_use]
pub fn image_path_from_pasted_text(text: &str) -> Option<PathBuf> {
    if text.contains(['\n', '\r']) {
        return None;
    }
    let candidate = strip_matching_quotes(text.trim());
    if candidate.is_empty() {
        return None;
    }
    let path = if candidate.starts_with("file://") {
        Url::parse(candidate).ok()?.to_file_path().ok()?
    } else {
        PathBuf::from(candidate)
    };
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp") || !path.exists() {
        return None;
    }
    Some(path)
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(
            (bytes[0], bytes[value.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        ) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
#[path = "tests/image_attachment_tests.rs"]
mod tests;
