use super::*;

#[test]
fn darwin_dataless_flag_is_detected_without_matching_unrelated_flags() {
    assert!(darwin_file_flags_are_dataless(DARWIN_SF_DATALESS));
    assert!(darwin_file_flags_are_dataless(DARWIN_SF_DATALESS | 1));
    assert!(!darwin_file_flags_are_dataless(0));
    assert!(!darwin_file_flags_are_dataless(1));
}

#[test]
fn definition_reader_accepts_bounded_utf8_regular_file() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("agent.md");
    std::fs::write(&path, "# Agent\n")?;

    assert_eq!(read_definition_text(&path)?, "# Agent\n");
    Ok(())
}

#[test]
fn definition_reader_rejects_non_regular_files_before_opening() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;

    assert_eq!(
        read_definition_file(temp.path()),
        Err(DefinitionFileReadError::Unavailable)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn definition_reader_rejects_fifo_without_waiting_for_a_writer() -> anyhow::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("agent.md");
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `path` is a live NUL-terminated path inside the owned temporary directory.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0);

    assert_eq!(
        read_definition_file(std::path::Path::new(path.to_str()?)),
        Err(DefinitionFileReadError::Unavailable)
    );
    Ok(())
}
