use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::FileExt;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn read_bounded(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > max_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("durable file {} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("durable file {} exceeds {max_bytes} bytes", path.display()),
        ));
    }
    Ok(bytes)
}

pub(crate) fn canonical_durable_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if fs::symlink_metadata(&path).is_ok() {
        return path.canonicalize();
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("durable path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("durable path has no file name: {}", path.display()),
        )
    })?;
    Ok(canonical_parent.join(name))
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let suffix = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temp = path_with_suffix(path, &format!(".tmp-{}-{suffix}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_and_sync(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(unix)]
fn replace_and_sync(temp: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temp, path)?;
    let parent = durable_parent(path)?;
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn replace_and_sync(temp: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = temp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // `std::fs::rename` cannot request `MOVEFILE_WRITE_THROUGH`, so retain one direct Win32 call
    // to preserve the durable replacement contract.
    // SAFETY: both vectors are non-null, NUL-terminated UTF-16 buffers. Their storage remains
    // stable and readable for the full call, and Win32 does not retain either pointer afterward.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_and_sync(temp: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temp, path)?;
    let parent = durable_parent(path)?;
    File::open(parent)?.sync_all()
}

#[cfg(not(windows))]
fn durable_parent(path: &Path) -> std::io::Result<&Path> {
    path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("durable path has no parent: {}", path.display()),
        )
    })
}

pub(crate) fn acquire_exclusive_lease(path: &Path) -> std::io::Result<File> {
    let lease_path = path_with_suffix(path, ".lock");
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lease_path)?;
    lease.try_lock_exclusive().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "failed to acquire durable lease {}: {error}",
                lease_path.display()
            ),
        )
    })?;
    Ok(lease)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
