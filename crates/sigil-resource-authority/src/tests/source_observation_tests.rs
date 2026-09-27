use std::{fs, path::Path};

use super::{HostSourceEntryObservation, HostSourceObservationRoot};

#[test]
fn host_source_observation_bounds_reads_and_reports_complete_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("note.txt"), "source")?;
    fs::write(root.path().join("other.txt"), "other")?;
    let observer = HostSourceObservationRoot::open(root.path())?;
    let (names, complete) = observer.directory_names(Path::new(""), 1)?;
    assert_eq!(names.len(), 1);
    assert!(!complete);
    let (_, complete) = observer.directory_names(Path::new(""), 10)?;
    assert!(complete);
    let HostSourceEntryObservation::File {
        content_digest,
        observed_bytes,
    } = observer.entry(Path::new("note.txt"), 6)?
    else {
        panic!("expected file")
    };
    assert!(content_digest.is_some());
    assert_eq!(observed_bytes, 6);
    let HostSourceEntryObservation::File {
        content_digest,
        observed_bytes,
    } = observer.entry(Path::new("note.txt"), 5)?
    else {
        panic!("expected file")
    };
    assert!(content_digest.is_none());
    assert_eq!(observed_bytes, 0);
    assert!(observer.entry(Path::new("../note.txt"), 10).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn host_source_observation_refuses_aliases_and_replaced_ancestors()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir(&outside)?;
    fs::write(root.join("src/note.txt"), "inside")?;
    fs::write(outside.join("note.txt"), "outside")?;
    let observer = HostSourceObservationRoot::open(&root)?;
    let (_, complete) = observer.directory_names(Path::new("src"), 10)?;
    assert!(complete);
    fs::rename(root.join("src"), root.join("old-src"))?;
    symlink(&outside, root.join("src"))?;
    assert!(observer.entry(Path::new("src/note.txt"), 100).is_err());
    assert!(observer.directory_names(Path::new("src"), 10).is_err());
    fs::hard_link(outside.join("note.txt"), root.join("hard-link.txt"))?;
    assert!(observer.entry(Path::new("hard-link.txt"), 100).is_err());
    fs::rename(&root, temp.path().join("moved-workspace"))?;
    fs::create_dir(&root)?;
    assert!(observer.directory_names(Path::new(""), 10).is_err());
    Ok(())
}
