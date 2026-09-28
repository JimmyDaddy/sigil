use super::*;

fn attachment(id: &str, bytes: u64) -> DesktopImageAttachment {
    DesktopImageAttachment {
        attachment_id: id.into(),
        sha256: "a".repeat(64),
        mime_type: "png".into(),
        width: 2,
        height: 3,
        byte_len: bytes,
        estimated_visual_tokens: 1,
        artifact_ref: format!("{}.png", "a".repeat(64)),
    }
}

#[test]
fn image_draft_handles_are_workspace_bound_bounded_and_released() {
    let mut registry = DraftImageRegistry::default();
    registry
        .insert("workspace-a", attachment("image-1", 72))
        .expect("admit");
    assert_eq!(
        registry
            .resolve("workspace-a", &["image-1".into()])
            .expect("same scope")
            .len(),
        1
    );
    assert!(
        registry
            .resolve("workspace-b", &["image-1".into()])
            .is_err()
    );
    assert!(
        registry
            .resolve("workspace-a", &["image-1".into(), "image-1".into()])
            .is_err()
    );
    registry.clear_workspace("workspace-b");
    assert_eq!(registry.images.len(), 1);
    registry.clear_workspace("workspace-a");
    assert!(
        registry
            .resolve("workspace-a", &["image-1".into()])
            .is_err()
    );
    for index in 0..MAX_DRAFT_IMAGES {
        registry
            .insert("workspace-a", attachment(&format!("image-{index}"), 72))
            .expect("within bound");
    }
    assert!(
        registry
            .insert("workspace-a", attachment("overflow", 72))
            .is_err()
    );
    registry.clear_workspace("workspace-a");
    registry
        .insert("workspace-a", attachment("large", MAX_DRAFT_BYTES))
        .expect("byte limit");
    assert!(
        registry
            .insert("workspace-a", attachment("overflow", 1))
            .is_err()
    );
    assert!(registry.resolve("workspace-a", &["large".into()]).is_err());
}

#[test]
fn image_ipc_projection_contains_no_content_hash_artifact_ref_or_path() {
    let reference = ImageReference::from(attachment("image-1", 72));
    let value = serde_json::to_value(reference).expect("serialize narrow projection");
    assert_eq!(value["mimeType"], "image/png");
    assert!(value.get("sha256").is_none());
    assert!(value.get("artifactRef").is_none());
    assert!(value.get("path").is_none());
}

#[test]
fn selected_image_reader_rejects_nonregular_empty_and_oversize_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    assert!(read_selected_image(temp.path()).is_err());
    let path = temp.path().join("image.png");
    let png = include_bytes!("../../icons/32x32.png");
    std::fs::write(&path, png)?;
    assert_eq!(read_selected_image(&path)?, png);
    #[cfg(unix)]
    {
        // macOS sockaddr_un cannot hold the long isolated TMPDIR prefix. This fixture has its
        // own unique directory and Drop cleanup; it never reads user state or configuration.
        let socket_root = tempfile::Builder::new()
            .prefix("sigil-img-")
            .tempdir_in("/tmp")?;
        let socket = socket_root.path().join("s");
        let _listener = std::os::unix::net::UnixListener::bind(&socket)?;
        assert!(read_selected_image(&socket).is_err());
    }
    std::fs::write(&path, [])?;
    assert!(read_selected_image(&path).is_err());
    std::fs::File::create(&path)?.set_len(MAX_DESKTOP_IMAGE_BYTES as u64 + 1)?;
    assert!(read_selected_image(&path).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn selected_image_symlink_ablation_removes_only_the_picker_target_gate()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;

    let temp = tempfile::tempdir()?;
    let target = temp.path().join("image.png");
    let selected = temp.path().join("selected-link.png");
    let png = include_bytes!("../../icons/32x32.png");
    std::fs::write(&target, png)?;
    std::os::unix::fs::symlink(&target, &selected)?;

    // Execute both removed checks against the same real picker target.
    assert!(
        std::fs::symlink_metadata(&selected)?
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&selected)
            .is_err()
    );
    assert_eq!(read_selected_image(&selected)?, png);
    std::fs::File::create(&target)?.set_len(MAX_DESKTOP_IMAGE_BYTES as u64 + 1)?;
    assert!(read_selected_image(&selected).is_err());
    std::fs::remove_file(&target)?;
    std::fs::create_dir(&target)?;
    assert!(read_selected_image(&selected).is_err());
    Ok(())
}

#[test]
fn same_image_bytes_in_two_drafts_keep_independent_release_ownership() {
    let mut registry = DraftImageRegistry::default();
    let first = attachment("first-admission", 72);
    let second = attachment("second-admission", 72);
    assert_eq!(first.artifact_ref, second.artifact_ref);
    registry.insert("workspace", first).expect("first draft");
    registry
        .insert("workspace", second.clone())
        .expect("second draft");
    registry.release("workspace", &["first-admission".into()]);
    assert!(
        registry
            .resolve("workspace", &["first-admission".into()])
            .is_err()
    );
    assert_eq!(
        registry
            .resolve("workspace", &["second-admission".into()])
            .expect("unrelated draft survives"),
        vec![second]
    );
}
