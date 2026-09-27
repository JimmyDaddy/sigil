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

#[test]
fn image_ingress_ablation_removes_one_redundant_decode_and_preserves_recovery_validation()
-> Result<()> {
    // Restore the pre-A3 cache-hit algorithm as the control: ingress decode + full cached decode.
    // The experiment uses actual PNG decoding and the same persisted bytes in both conditions.
    let temp = tempfile::tempdir()?;
    let cache = ControlledImageAttachmentCache::new(temp.path().join("attachments"));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(1920, 1080).write_to(&mut encoded, ImageFormat::Png)?;
    let bytes = encoded.into_inner();
    let expected = cache.ingest_encoded_bytes("experiment", bytes.clone())?;
    let iterations = 5;
    IMAGE_DECODE_COUNT.with(|count| count.set(0));
    let baseline_started = std::time::Instant::now();
    for _ in 0..iterations {
        let identified = identify_and_decode_image(&bytes)?;
        let attachment = sigil_kernel::ImageAttachment::from_bytes(
            "experiment",
            identified.mime_type,
            identified.width,
            identified.height,
            bytes.clone(),
        )?;
        cache.verify_cached_attachment(&attachment)?;
        assert_eq!(
            attachment.without_resolved_bytes(),
            expected.without_resolved_bytes()
        );
    }
    let baseline_elapsed = baseline_started.elapsed();
    let baseline_decodes = IMAGE_DECODE_COUNT.with(std::cell::Cell::get);
    IMAGE_DECODE_COUNT.with(|count| count.set(0));
    let optimized_started = std::time::Instant::now();
    for _ in 0..iterations {
        let attachment = cache.ingest_encoded_bytes("experiment", bytes.clone())?;
        assert_eq!(
            attachment.without_resolved_bytes(),
            expected.without_resolved_bytes()
        );
    }
    let optimized_elapsed = optimized_started.elapsed();
    let optimized_decodes = IMAGE_DECODE_COUNT.with(std::cell::Cell::get);
    assert_eq!(baseline_decodes, iterations * 2);
    assert_eq!(optimized_decodes, iterations);
    eprintln!(
        "A3 image ingress ablation: iterations={iterations} baseline_decodes={baseline_decodes} optimized_decodes={optimized_decodes} baseline_ms={} optimized_ms={}",
        baseline_elapsed.as_millis(),
        optimized_elapsed.as_millis()
    );

    // A durable record is untrusted input: dimensions/MIME must still match decoded cached bytes.
    let mut forged = expected.without_resolved_bytes();
    forged.width += 1;
    forged.estimated_visual_tokens =
        sigil_kernel::estimate_visual_tokens(forged.width, forged.height);
    assert!(cache.resolve(&forged).is_err());
    assert_eq!(cache.resolve(&expected.without_resolved_bytes())?, bytes);
    Ok(())
}
