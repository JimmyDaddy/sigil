use std::{fs, io::Cursor};

use anyhow::Result;
use image::{DynamicImage, ImageFormat};
use sigil_kernel::{ImageAttachmentResolver, MAX_IMAGE_ATTACHMENT_BYTES};

use super::*;

fn png_bytes() -> Result<Vec<u8>> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 3).write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

#[test]
fn cache_ingress_is_content_addressed_and_resolves_verified_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = ControlledImageAttachmentCache::new(temp.path().join("attachments"));
    let bytes = png_bytes()?;

    let first = cache.ingest_encoded_bytes("image-1", bytes.clone())?;
    let second = cache.ingest_encoded_bytes("image-2", bytes.clone())?;

    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.artifact_ref, second.artifact_ref);
    assert_eq!(first.width, 2);
    assert_eq!(first.height, 3);
    assert_eq!(cache.resolve(&first.without_resolved_bytes())?, bytes);
    assert_eq!(fs::read_dir(cache.root())?.count(), 1);
    Ok(())
}

#[test]
fn cache_rejects_tamper_wrong_format_and_oversized_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = ControlledImageAttachmentCache::new(temp.path().join("attachments"));
    let attachment = cache.ingest_encoded_bytes("image-1", png_bytes()?)?;
    fs::write(cache.root().join(&attachment.artifact_ref), b"not an image")?;
    let error = cache
        .resolve(&attachment.without_resolved_bytes())
        .expect_err("tampered cache blob must fail");
    assert!(error.to_string().contains("format") || error.to_string().contains("length"));

    let error = cache
        .ingest_encoded_bytes("image-2", b"plain text".to_vec())
        .expect_err("unsupported input must fail");
    assert!(error.to_string().contains("format"));

    let oversized = temp.path().join("oversized.png");
    fs::File::create(&oversized)?.set_len(MAX_IMAGE_ATTACHMENT_BYTES + 1)?;
    let error = cache
        .ingest_path("image-3", &oversized)
        .expect_err("oversized source must fail before decode");
    assert!(format!("{error:#}").contains("V1 limit"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn cache_rejects_symlink_source_leaf_and_root() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source.png");
    fs::write(&source, png_bytes()?)?;
    let linked_source = temp.path().join("linked.png");
    symlink(&source, &linked_source)?;
    let cache = ControlledImageAttachmentCache::new(temp.path().join("attachments"));
    let error = cache
        .ingest_path("image-1", &linked_source)
        .expect_err("symlink source must fail");
    assert!(format!("{error:#}").contains("no-follow"));

    let real_root = temp.path().join("real-root");
    fs::create_dir(&real_root)?;
    let linked_root = temp.path().join("linked-root");
    symlink(&real_root, &linked_root)?;
    let cache = ControlledImageAttachmentCache::new(linked_root);
    let error = cache
        .ingest_encoded_bytes("image-2", png_bytes()?)
        .expect_err("symlink cache root must fail");
    assert!(error.to_string().contains("cache root"));
    Ok(())
}

#[test]
fn pasted_image_path_recognizes_single_supported_path_and_file_url() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("screen shot.png");
    fs::write(&path, png_bytes()?)?;

    assert_eq!(
        image_path_from_pasted_text(&format!("\"{}\"", path.display())),
        Some(path.clone())
    );
    assert_eq!(
        image_path_from_pasted_text(url::Url::from_file_path(&path).expect("file URL").as_str()),
        Some(path)
    );
    assert!(image_path_from_pasted_text("ordinary prompt").is_none());
    assert!(image_path_from_pasted_text("one.png\ntwo.png").is_none());
    Ok(())
}
