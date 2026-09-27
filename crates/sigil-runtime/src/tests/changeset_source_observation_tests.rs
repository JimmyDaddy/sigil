use std::{fs, path::Path};

use anyhow::Result;
use sigil_kernel::{ChangeSet, ChangeSetFileAction, decode_changeset_only_child_output};

use super::{ChangesetSourceObservation, MAX_SOURCE_FILE_BYTES};

fn proposal(path: &str, action: &str) -> Result<ChangeSet> {
    Ok(decode_changeset_only_child_output(&serde_json::json!({
        "change_set": { "id": "source-change", "title": "test", "summary": "test", "risk": "low", "files": [{ "path": path, "action": action, "risk": "low", "additions": 1, "deletions": 1 }], "validations": [] },
        "artifact": { "media_type": "text/x-diff", "content": "patch" }
    }).to_string())?.change_set)
}

#[tokio::test]
async fn changeset_source_observation_keeps_unrelated_unknowns_out_of_target_binding() -> Result<()>
{
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("note.txt"), b"old\n")?;
    fs::File::create(root.path().join("large.bin"))?.set_len(MAX_SOURCE_FILE_BYTES + 1)?;
    fs::create_dir(root.path().join("target"))?;
    fs::write(root.path().join("target/generated.txt"), "generated")?;
    let source = ChangesetSourceObservation::capture(root.path()).await?;
    assert_eq!(
        source.source_hash(Path::new("note.txt"))?,
        Some(sigil_kernel::bytes_hash(b"old\n"))
    );
    assert!(source.source_hash(Path::new("large.bin")).is_err());
    assert!(
        source
            .source_hash(Path::new("target/generated.txt"))
            .is_err()
    );
    assert_eq!(source.source_hash(Path::new("new/nested.txt"))?, None);
    let mut change = proposal("note.txt", "update")?;
    fs::write(root.path().join("note.txt"), b"user change\n")?;
    change.files[0].before_hash = Some(sigil_kernel::bytes_hash(b"user change\n"));
    let binding = source.bind(root.path(), &mut change, "patch")?;
    assert!(binding.snapshot_id().is_ok());
    assert_eq!(
        change.files[0].before_hash,
        Some(sigil_kernel::bytes_hash(b"old\n"))
    );
    let mut unknown = proposal("large.bin", "update")?;
    assert!(source.bind(root.path(), &mut unknown, "patch").is_err());
    let mut absent = proposal("new/nested.txt", "create")?;
    assert!(source.bind(root.path(), &mut absent, "patch").is_ok());
    absent.files[0].action = ChangeSetFileAction::Update;
    assert!(source.bind(root.path(), &mut absent, "patch").is_err());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn changeset_source_observation_preserves_unknown_aliased_subtree() -> Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(root.path().join("note.txt"), b"old\n")?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("alias"))?;
    let source = ChangesetSourceObservation::capture(root.path()).await?;
    assert!(source.source_hash(Path::new("alias/new.txt")).is_err());
    assert!(source.source_hash(Path::new("note.txt")).is_ok());
    Ok(())
}
