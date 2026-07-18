use std::{
    ffi::{OsStr, c_void},
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    mem::{size_of, size_of_val},
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACL,
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetEffectiveRightsFromAclW, GetNamedSecurityInfoW,
            SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
            TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetFileSecurityW,
        GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_DELETE_CHILD,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetFileInformationByHandle,
        GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        WRITE_OWNER,
    },
};

use super::native::WindowsRestrictingSid;

const JOURNAL_VERSION: u32 = 1;
const DRIVE_REMOTE: u32 = 4;
const ACCESS_SYSTEM_SECURITY: u32 = 0x0100_0000;
const ROOT_SECURITY_INFORMATION: u32 =
    OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
const ROOT_GRANT_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;
const FORBIDDEN_ROOT_GRANT_ACCESS: u32 = WRITE_DAC | WRITE_OWNER | ACCESS_SYSTEM_SECURITY;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RootIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct GrantJournal {
    version: u32,
    owner_marker: String,
    root_utf16: Vec<u16>,
    root_identity: RootIdentity,
    restricting_sid: String,
    snapshot_sha256: String,
    snapshot_len: u64,
    phase: String,
}

struct SecurityDescriptorBuffer {
    storage: Vec<usize>,
    len: usize,
}

impl SecurityDescriptorBuffer {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            bail!("Windows security descriptor snapshot is empty");
        }
        let words = bytes.len().div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        // SAFETY: storage has at least bytes.len() writable bytes and does not overlap bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                storage.as_mut_ptr().cast::<u8>(),
                bytes.len(),
            );
        }
        Ok(Self {
            storage,
            len: bytes.len(),
        })
    }

    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.storage.as_ptr().cast_mut().cast::<c_void>()
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: storage contains at least len initialized bytes and remains live for the slice.
        unsafe { std::slice::from_raw_parts(self.storage.as_ptr().cast::<u8>(), self.len) }
    }
}

struct GrantPaths {
    lock: PathBuf,
    journal: PathBuf,
    snapshot: PathBuf,
    granted: PathBuf,
    restoring: PathBuf,
    restored: PathBuf,
}

impl GrantPaths {
    fn for_root(state_dir: &Path, root_utf16: &[u16]) -> Self {
        let mut root_bytes = Vec::with_capacity(size_of_val(root_utf16));
        for unit in root_utf16 {
            root_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let root_key = sha256_hex(&root_bytes);
        Self {
            lock: state_dir.join(format!("{root_key}.lock")),
            journal: state_dir.join(format!("{root_key}.journal.json")),
            snapshot: state_dir.join(format!("{root_key}.security-descriptor")),
            granted: state_dir.join(format!("{root_key}.granted")),
            restoring: state_dir.join(format!("{root_key}.restoring")),
            restored: state_dir.join(format!("{root_key}.restored")),
        }
    }
}

/// Private R41.2 lease for one temporary root DACL grant.
///
/// Construction records the exact pre-mutation security descriptor before applying an ACE.
/// Explicit restoration is required for conformance; Drop only provides a best-effort retry.
pub(crate) struct WindowsFilesystemGrant {
    root: PathBuf,
    root_identity: RootIdentity,
    paths: GrantPaths,
    lock: File,
    active: bool,
}

impl WindowsFilesystemGrant {
    pub(crate) fn acquire(
        root: &Path,
        state_dir: &Path,
        restricting_sid: &WindowsRestrictingSid,
    ) -> Result<Self> {
        assert_minimal_root_grant()?;
        let root = canonical_directory(root, "grant root")?;
        ensure_supported_local_ntfs(&root)?;
        let root_utf16 = wide_without_nul(root.as_os_str());
        let root_identity = root_identity(&root)?;
        let state_dir = canonical_state_directory(state_dir, &root)?;
        let paths = GrantPaths::for_root(&state_dir, &root_utf16);
        let lock = open_and_lock(&paths.lock)?;
        recover_existing_locked(&root, &root_utf16, root_identity, &paths)?;

        let original = read_security_descriptor(&root)?;
        let snapshot_sha256 = sha256_hex(original.as_bytes());
        write_new_synced(&paths.snapshot, original.as_bytes())?;
        let journal = GrantJournal {
            version: JOURNAL_VERSION,
            owner_marker: uuid::Uuid::new_v4().to_string(),
            root_utf16,
            root_identity,
            restricting_sid: restricting_sid.as_str().to_owned(),
            snapshot_sha256,
            snapshot_len: u64::try_from(original.len)
                .context("security descriptor length exceeds u64")?,
            phase: "prepared".to_owned(),
        };
        let journal_bytes = serde_json::to_vec(&journal).context("failed to encode ACL journal")?;
        if let Err(error) = write_new_synced(&paths.journal, &journal_bytes) {
            let _ = remove_if_exists(&paths.snapshot);
            return Err(error);
        }

        let apply_result = apply_and_verify_root_grant(&root, restricting_sid);
        if let Err(error) = apply_result {
            return rollback_failed_acquire(&root, root_identity, &paths, error);
        }
        if let Err(error) = write_new_synced(&paths.granted, b"granted\n") {
            return rollback_failed_acquire(&root, root_identity, &paths, error);
        }

        Ok(Self {
            root,
            root_identity,
            paths,
            lock,
            active: true,
        })
    }

    pub(crate) fn recover(root: &Path, state_dir: &Path) -> Result<bool> {
        let root = canonical_directory(root, "recovery root")?;
        ensure_supported_local_ntfs(&root)?;
        let root_utf16 = wide_without_nul(root.as_os_str());
        let root_identity = root_identity(&root)?;
        let state_dir = canonical_state_directory(state_dir, &root)?;
        let paths = GrantPaths::for_root(&state_dir, &root_utf16);
        let lock = open_and_lock(&paths.lock)?;
        let had_record = paths.journal.exists() || paths.snapshot.exists();
        let result = recover_existing_locked(&root, &root_utf16, root_identity, &paths);
        let _ = FileExt::unlock(&lock);
        result.map(|()| had_record)
    }

    pub(crate) fn restore(mut self) -> Result<()> {
        let result = self.restore_active();
        if result.is_ok() {
            self.active = false;
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn descriptor_hash(path: &Path) -> Result<String> {
        Ok(sha256_hex(read_security_descriptor(path)?.as_bytes()))
    }

    #[cfg(test)]
    pub(crate) fn sid_has_mutating_rights(
        path: &Path,
        restricting_sid: &WindowsRestrictingSid,
    ) -> Result<bool> {
        let effective = effective_rights_for_path(path, restricting_sid)?;
        let mutating = FILE_GENERIC_WRITE
            | DELETE
            | FILE_DELETE_CHILD
            | WRITE_DAC
            | WRITE_OWNER
            | ACCESS_SYSTEM_SECURITY;
        Ok(effective & mutating != 0)
    }

    fn restore_active(&mut self) -> Result<()> {
        verify_root_identity(&self.root, self.root_identity)?;
        write_marker_if_absent(&self.paths.restoring, b"restoring\n")?;
        restore_from_snapshot(&self.root, &self.paths)?;
        verify_restored_snapshot(&self.root, &self.paths)?;
        write_marker_if_absent(&self.paths.restored, b"restored\n")?;
        clear_grant_artifacts(&self.paths)
    }
}

impl Drop for WindowsFilesystemGrant {
    fn drop(&mut self) {
        if self.active && self.restore_active().is_ok() {
            self.active = false;
        }
        let _ = FileExt::unlock(&self.lock);
    }
}

fn rollback_failed_acquire(
    root: &Path,
    root_identity: RootIdentity,
    paths: &GrantPaths,
    source: anyhow::Error,
) -> Result<WindowsFilesystemGrant> {
    let rollback = verify_root_identity(root, root_identity)
        .and_then(|()| restore_from_snapshot(root, paths))
        .and_then(|()| verify_restored_snapshot(root, paths))
        .and_then(|()| clear_grant_artifacts(paths));
    match rollback {
        Ok(()) => Err(source.context("Windows root grant failed and was restored")),
        Err(rollback_error) => Err(source.context(format!(
            "Windows root grant failed; rollback also failed: {rollback_error:#}"
        ))),
    }
}

fn recover_existing_locked(
    root: &Path,
    root_utf16: &[u16],
    root_identity: RootIdentity,
    paths: &GrantPaths,
) -> Result<()> {
    if !paths.journal.exists() {
        if paths.granted.exists() || paths.restoring.exists() || paths.restored.exists() {
            bail!("Windows ACL recovery markers exist without an owner journal");
        }
        if paths.snapshot.exists() {
            // Snapshot publication precedes journal publication, so a snapshot without a journal
            // proves that mutation was not yet allowed to begin.
            remove_if_exists(&paths.snapshot)?;
        }
        return Ok(());
    }

    let journal_bytes = fs::read(&paths.journal)
        .with_context(|| format!("failed to read {}", paths.journal.display()))?;
    let journal: GrantJournal = serde_json::from_slice(&journal_bytes)
        .with_context(|| format!("failed to parse {}", paths.journal.display()))?;
    if journal.version != JOURNAL_VERSION
        || journal.phase != "prepared"
        || journal.root_utf16 != root_utf16
        || journal.root_identity != root_identity
        || journal.owner_marker.is_empty()
        || journal.restricting_sid.is_empty()
    {
        bail!("Windows ACL recovery journal does not match the canonical root identity");
    }
    verify_snapshot_metadata(paths, &journal)?;
    verify_root_identity(root, root_identity)?;
    write_marker_if_absent(&paths.restoring, b"restoring\n")?;
    restore_from_snapshot(root, paths)?;
    verify_restored_snapshot(root, paths)?;
    write_marker_if_absent(&paths.restored, b"restored\n")?;
    clear_grant_artifacts(paths)
}

fn verify_snapshot_metadata(paths: &GrantPaths, journal: &GrantJournal) -> Result<()> {
    let snapshot = fs::read(&paths.snapshot)
        .with_context(|| format!("failed to read {}", paths.snapshot.display()))?;
    if u64::try_from(snapshot.len()).context("snapshot length exceeds u64")? != journal.snapshot_len
        || sha256_hex(&snapshot) != journal.snapshot_sha256
    {
        bail!("Windows ACL recovery snapshot does not match its durable journal");
    }
    Ok(())
}

fn apply_and_verify_root_grant(root: &Path, restricting_sid: &WindowsRestrictingSid) -> Result<()> {
    let wide = nul_terminated(root.as_os_str(), "grant root")?;
    let mut current_dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: wide is NUL-terminated and output pointers are valid. descriptor is released with
    // LocalFree after all pointers into it are no longer needed.
    let read_status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &raw mut current_dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if read_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(read_status.cast_signed()))
            .context("GetNamedSecurityInfoW failed for Windows grant root");
    }

    let explicit = EXPLICIT_ACCESS_W {
        grfAccessPermissions: ROOT_GRANT_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: trustee_for_sid(restricting_sid.as_ptr()),
    };
    let mut updated_dacl: *mut ACL = null_mut();
    // SAFETY: explicit and its SID remain live, current_dacl points into descriptor, and
    // updated_dacl is a valid output pointer.
    let merge_status =
        unsafe { SetEntriesInAclW(1, &raw const explicit, current_dacl, &raw mut updated_dacl) };
    if merge_status != ERROR_SUCCESS {
        free_local(descriptor);
        return Err(io::Error::from_raw_os_error(merge_status.cast_signed()))
            .context("SetEntriesInAclW failed for Windows grant root");
    }
    if updated_dacl.is_null() {
        free_local(descriptor);
        bail!("SetEntriesInAclW returned a null Windows grant root DACL");
    }

    // SAFETY: all pointers are valid for the call; only the DACL is replaced.
    let set_status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            updated_dacl,
            null(),
        )
    };
    free_local(updated_dacl.cast::<c_void>());
    free_local(descriptor);
    if set_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(set_status.cast_signed()))
            .context("SetNamedSecurityInfoW failed for Windows grant root");
    }

    verify_effective_root_grant(root, restricting_sid)
}

fn verify_effective_root_grant(root: &Path, restricting_sid: &WindowsRestrictingSid) -> Result<()> {
    let effective = effective_rights_for_path(root, restricting_sid)?;
    if effective & ROOT_GRANT_ACCESS != ROOT_GRANT_ACCESS {
        bail!("Windows grant root did not expose the required minimal effective rights");
    }
    if effective & FORBIDDEN_ROOT_GRANT_ACCESS != 0 {
        bail!("Windows grant root exposed forbidden owner or ACL mutation rights");
    }
    Ok(())
}

fn effective_rights_for_path(path: &Path, restricting_sid: &WindowsRestrictingSid) -> Result<u32> {
    let wide = nul_terminated(path.as_os_str(), "effective-rights path")?;
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: wide and all output pointers remain valid for the call.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()))
            .context("failed to reread Windows grant root DACL");
    }
    if dacl.is_null() {
        free_local(descriptor);
        bail!("Windows grant root unexpectedly has a null DACL");
    }

    let trustee = trustee_for_sid(restricting_sid.as_ptr());
    let mut effective = 0_u32;
    // SAFETY: dacl points into the live descriptor and trustee references the live SID.
    let rights_status =
        unsafe { GetEffectiveRightsFromAclW(dacl, &raw const trustee, &raw mut effective) };
    free_local(descriptor);
    if rights_status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rights_status.cast_signed()))
            .context("failed to resolve effective Windows filesystem rights");
    }
    Ok(effective)
}

fn trustee_for_sid(sid: *mut c_void) -> TRUSTEE_W {
    TRUSTEE_W {
        pMultipleTrustee: null_mut(),
        MultipleTrusteeOperation: 0,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        ptstrName: sid.cast::<u16>(),
    }
}

fn restore_from_snapshot(root: &Path, paths: &GrantPaths) -> Result<()> {
    let bytes = fs::read(&paths.snapshot)
        .with_context(|| format!("failed to read {}", paths.snapshot.display()))?;
    let descriptor = SecurityDescriptorBuffer::from_bytes(&bytes)?;
    let wide = nul_terminated(root.as_os_str(), "grant root")?;
    let mut dacl_present = 0;
    let mut dacl_defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: descriptor is an aligned, self-relative security descriptor captured from
    // GetFileSecurityW, and all output pointers are valid for the call.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor.as_ptr(),
            &raw mut dacl_present,
            &raw mut dacl,
            &raw mut dacl_defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error())
            .context("GetSecurityDescriptorDacl failed for Windows grant snapshot");
    }
    if dacl_present == 0 || dacl.is_null() {
        bail!("Windows grant root with an absent or null original DACL is unsupported");
    }

    // SAFETY: wide is NUL-terminated and dacl points into the live snapshot descriptor.
    // SetNamedSecurityInfoW is used intentionally because it propagates the restored inheritable
    // DACL to existing descendants, unlike SetFileSecurityW's root-only replacement.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()))
            .context("SetNamedSecurityInfoW failed to restore Windows grant root DACL");
    }
    Ok(())
}

fn verify_restored_snapshot(root: &Path, paths: &GrantPaths) -> Result<()> {
    let expected = fs::read(&paths.snapshot)
        .with_context(|| format!("failed to read {}", paths.snapshot.display()))?;
    let actual = read_security_descriptor(root)?;
    if actual.as_bytes() != expected {
        bail!("Windows grant root security descriptor did not restore exactly");
    }
    Ok(())
}

fn read_security_descriptor(path: &Path) -> Result<SecurityDescriptorBuffer> {
    let wide = nul_terminated(path.as_os_str(), "security descriptor path")?;
    let mut bytes = 0_u32;
    // SAFETY: This is the documented sizing call and bytes is a valid output pointer.
    let _ = unsafe {
        GetFileSecurityW(
            wide.as_ptr(),
            ROOT_SECURITY_INFORMATION,
            null_mut(),
            0,
            &raw mut bytes,
        )
    };
    if bytes == 0 {
        return Err(io::Error::last_os_error())
            .context("GetFileSecurityW failed to size Windows grant root descriptor");
    }
    let len = usize::try_from(bytes).context("security descriptor length exceeds usize")?;
    let words = len.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; words];
    let capacity = u32::try_from(storage.len() * size_of::<usize>())
        .context("security descriptor buffer exceeds u32")?;
    let mut returned = 0_u32;
    // SAFETY: storage is aligned and sufficiently sized; returned is a valid output pointer.
    if unsafe {
        GetFileSecurityW(
            wide.as_ptr(),
            ROOT_SECURITY_INFORMATION,
            storage.as_mut_ptr().cast::<c_void>(),
            capacity,
            &raw mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error())
            .context("GetFileSecurityW failed for Windows grant root");
    }
    let returned = usize::try_from(returned).context("security descriptor length exceeds usize")?;
    if returned == 0 || returned > storage.len() * size_of::<usize>() {
        bail!("GetFileSecurityW returned an invalid descriptor length");
    }
    Ok(SecurityDescriptorBuffer {
        storage,
        len: returned,
    })
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize Windows {label}"))?;
    if !canonical.is_dir() {
        bail!(
            "Windows {label} is not a directory: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

fn canonical_state_directory(state_dir: &Path, root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create Windows ACL state {}", state_dir.display()))?;
    let state_dir = canonical_directory(state_dir, "ACL state directory")?;
    if state_dir.starts_with(root) {
        bail!("Windows ACL state directory must be outside the granted root");
    }
    Ok(state_dir)
}

fn ensure_supported_local_ntfs(root: &Path) -> Result<()> {
    let root_wide = nul_terminated(root.as_os_str(), "grant root")?;
    let mut volume_path = vec![0_u16; 32_768];
    // SAFETY: root_wide is NUL-terminated and volume_path is a writable fixed-size buffer.
    if unsafe {
        GetVolumePathNameW(
            root_wide.as_ptr(),
            volume_path.as_mut_ptr(),
            u32::try_from(volume_path.len()).expect("volume path buffer fits u32"),
        )
    } == 0
    {
        return Err(io::Error::last_os_error())
            .context("GetVolumePathNameW failed for Windows grant root");
    }
    // SAFETY: GetVolumePathNameW wrote a NUL-terminated volume path into the buffer.
    if unsafe { GetDriveTypeW(volume_path.as_ptr()) } == DRIVE_REMOTE {
        bail!("Windows restricted root on a remote volume is unsupported");
    }

    let mut filesystem_name = [0_u16; 32];
    // SAFETY: volume_path is NUL-terminated, optional output pointers are null, and
    // filesystem_name is a valid writable buffer.
    if unsafe {
        GetVolumeInformationW(
            volume_path.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem_name.as_mut_ptr(),
            u32::try_from(filesystem_name.len()).expect("filesystem name buffer fits u32"),
        )
    } == 0
    {
        return Err(io::Error::last_os_error())
            .context("GetVolumeInformationW failed for Windows grant root");
    }
    let name_len = filesystem_name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem_name.len());
    let filesystem_name = String::from_utf16_lossy(&filesystem_name[..name_len]);
    if !filesystem_name.eq_ignore_ascii_case("NTFS") {
        bail!("Windows restricted root requires NTFS; found {filesystem_name}");
    }
    Ok(())
}

fn root_identity(root: &Path) -> Result<RootIdentity> {
    use std::os::windows::io::AsRawHandle;

    let wide = nul_terminated(root.as_os_str(), "grant root")?;
    // SAFETY: wide is NUL-terminated. The returned handle is checked and then owned by File.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error())
            .context("CreateFileW failed for Windows grant root identity");
    }
    // SAFETY: CreateFileW returned an owned, non-sentinel handle.
    let file = unsafe { File::from_raw_handle(raw) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file is live and info is a valid output pointer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error())
            .context("GetFileInformationByHandle failed for Windows grant root");
    }
    Ok(RootIdentity {
        volume_serial_number: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

fn verify_root_identity(root: &Path, expected: RootIdentity) -> Result<()> {
    if root_identity(root)? != expected {
        bail!("Windows grant root identity changed before ACL recovery");
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
        .with_context(|| format!("failed to open Windows ACL lease {}", path.display()))?;
    file.try_lock_exclusive()
        .with_context(|| format!("Windows ACL lease is already held for {}", path.display()))?;
    Ok(file)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| {
            format!(
                "failed to create durable Windows ACL record {}",
                path.display()
            )
        })?;
    file.write_all(bytes).with_context(|| {
        format!(
            "failed to write durable Windows ACL record {}",
            path.display()
        )
    })?;
    file.sync_all().with_context(|| {
        format!(
            "failed to sync durable Windows ACL record {}",
            path.display()
        )
    })
}

fn write_marker_if_absent(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    write_new_synced(path, bytes)
}

fn clear_grant_artifacts(paths: &GrantPaths) -> Result<()> {
    remove_if_exists(&paths.granted)?;
    remove_if_exists(&paths.restoring)?;
    remove_if_exists(&paths.restored)?;
    // Remove the owner journal before the snapshot. A crash between these removals leaves only a
    // pre-journal snapshot, which recovery can prove was safe to discard.
    remove_if_exists(&paths.journal)?;
    remove_if_exists(&paths.snapshot)
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn assert_minimal_root_grant() -> Result<()> {
    if ROOT_GRANT_ACCESS & FORBIDDEN_ROOT_GRANT_ACCESS != 0 {
        bail!("Windows restricted root grant contains forbidden security-control rights");
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn wide_without_nul(value: &OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

fn nul_terminated(value: &OsStr, label: &str) -> Result<Vec<u16>> {
    let mut wide = wide_without_nul(value);
    if wide.contains(&0) {
        bail!("Windows {label} contains an interior NUL");
    }
    wide.push(0);
    Ok(wide)
}

fn free_local(pointer: *mut c_void) {
    if !pointer.is_null() {
        // SAFETY: The pointer was allocated by a Win32 API documented to use LocalAlloc.
        let _ = unsafe { LocalFree(pointer) };
    }
}
