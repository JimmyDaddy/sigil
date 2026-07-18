use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};

use super::native::WindowsAppContainerSid;

const JOURNAL_VERSION: u32 = 1;
const PROFILE_NAME_PREFIX: &str = "Sigil.Rfc0041.";
const HRESULT_ALREADY_EXISTS: i32 = 0x8007_00B7_u32 as i32;

#[derive(Debug, Deserialize, Serialize)]
struct ProfileJournal {
    version: u32,
    owner_marker: String,
    profile_name: String,
    phase: String,
}

struct ProfilePaths {
    lock: PathBuf,
    journal: PathBuf,
}

impl ProfilePaths {
    fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            lock: state_dir.join("private-appcontainer.lock"),
            journal: state_dir.join("private-appcontainer.journal.json"),
        }
    }
}

/// Private R41.2 owner for a disposable AppContainer profile.
///
/// The owner journal is durable before profile creation. A later process holding the same
/// cross-process lease deletes a profile left by an interrupted owner before creating a new one.
/// Public backend work remains gated on the full filesystem containment matrix.
pub(crate) struct WindowsAppContainerProfile {
    name: String,
    sid: WindowsAppContainerSid,
    lock: File,
    journal: PathBuf,
    active: bool,
}

impl WindowsAppContainerProfile {
    pub(crate) fn create_private_probe(state_dir: &Path) -> Result<Self> {
        fs::create_dir_all(state_dir).with_context(|| {
            format!(
                "failed to create private AppContainer state {}",
                state_dir.display()
            )
        })?;
        let state_dir = fs::canonicalize(state_dir).with_context(|| {
            format!(
                "failed to canonicalize private AppContainer state {}",
                state_dir.display()
            )
        })?;
        let paths = ProfilePaths::in_state_dir(&state_dir);
        let lock = open_and_lock(&paths.lock)?;
        recover_interrupted_profile(&paths.journal)?;

        let owner_marker = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("{PROFILE_NAME_PREFIX}{owner_marker}");
        let journal = ProfileJournal {
            version: JOURNAL_VERSION,
            owner_marker,
            profile_name: name.clone(),
            phase: "prepared".to_owned(),
        };
        let journal_bytes =
            serde_json::to_vec(&journal).context("failed to encode AppContainer journal")?;
        write_new_synced(&paths.journal, &journal_bytes)?;

        let name_wide = nul_terminated(OsStr::new(&name), "AppContainer profile name")?;
        let display = nul_terminated(OsStr::new("Sigil R41.2 private probe"), "display name")?;
        let description = nul_terminated(
            OsStr::new("Disposable profile for hosted Windows containment proof"),
            "description",
        )?;
        let mut sid = null_mut();
        // SAFETY: all strings are NUL-terminated, no capabilities are requested, and sid is a
        // valid output pointer. Successful ownership transfers to WindowsAppContainerSid.
        let status = unsafe {
            CreateAppContainerProfile(
                name_wide.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                null(),
                0,
                &raw mut sid,
            )
        };
        if status != 0 {
            if status == HRESULT_ALREADY_EXISTS {
                remove_if_exists(&paths.journal)?;
                bail!("private AppContainer profile name unexpectedly already exists");
            }
            // The API documents profile state as potentially indeterminate after a failure. Keep
            // the owner journal so the next lease holder retries idempotent deletion.
            bail!("failed to create private AppContainer profile: HRESULT {status:#010x}");
        }
        let sid = WindowsAppContainerSid::from_owned(sid)?;

        Ok(Self {
            name,
            sid,
            lock,
            journal: paths.journal,
            active: true,
        })
    }

    pub(crate) fn sid(&self) -> &WindowsAppContainerSid {
        &self.sid
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    pub(crate) fn journal_path(&self) -> &Path {
        &self.journal
    }

    #[cfg(test)]
    pub(crate) fn abandon_for_recovery(mut self) -> Result<String> {
        FileExt::unlock(&self.lock).context("failed to abandon AppContainer profile lease")?;
        self.active = false;
        Ok(self.name.clone())
    }

    pub(crate) fn close(mut self) -> Result<()> {
        delete_profile(&self.name)?;
        remove_if_exists(&self.journal)?;
        FileExt::unlock(&self.lock).context("failed to release AppContainer profile lease")?;
        self.active = false;
        Ok(())
    }
}

impl Drop for WindowsAppContainerProfile {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if delete_profile(&self.name).is_ok() {
            let _ = remove_if_exists(&self.journal);
        }
        let _ = FileExt::unlock(&self.lock);
    }
}

fn recover_interrupted_profile(journal_path: &Path) -> Result<()> {
    let bytes = match fs::read(journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", journal_path.display()));
        }
    };
    let journal: ProfileJournal = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", journal_path.display()))?;
    if journal.version != JOURNAL_VERSION
        || journal.phase != "prepared"
        || journal.owner_marker.is_empty()
        || journal.profile_name != format!("{PROFILE_NAME_PREFIX}{}", journal.owner_marker)
    {
        bail!("private AppContainer recovery journal is not owned by this implementation");
    }
    delete_profile(&journal.profile_name)
        .context("failed to recover an interrupted private AppContainer profile")?;
    remove_if_exists(journal_path)
}

fn delete_profile(name: &str) -> Result<()> {
    let name = nul_terminated(OsStr::new(name), "AppContainer profile name")?;
    // SAFETY: name is NUL-terminated. Deletion of a non-existent profile is documented as S_OK.
    let status = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if status != 0 {
        bail!("failed to delete private AppContainer profile: HRESULT {status:#010x}");
    }
    Ok(())
}

fn open_and_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| {
            format!(
                "failed to open AppContainer profile lease {}",
                path.display()
            )
        })?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "AppContainer profile lease is already held for {}",
            path.display()
        )
    })?;
    Ok(file)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to create AppContainer journal {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write AppContainer journal {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync AppContainer journal {}", path.display()))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn nul_terminated(value: &OsStr, label: &str) -> Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        bail!("{label} contains an embedded NUL");
    }
    wide.push(0);
    Ok(wide)
}
