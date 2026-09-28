//! Bounded reads of files explicitly selected by the native picker. No path crosses IPC.

use std::{
    fs::OpenOptions,
    io::{self, Read},
    path::Path,
};

pub(crate) fn read_selected_file(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Follow the user-selected target, but do not block while opening a FIFO.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 != metadata.len() || bytes.len() as u64 > max_bytes {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(bytes)
}
