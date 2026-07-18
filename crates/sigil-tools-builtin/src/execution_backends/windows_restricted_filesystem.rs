use std::{
    ffi::{OsStr, c_void},
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
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
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetEffectiveRightsFromAclW, GetNamedSecurityInfoW,
            SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
            TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GROUP_SECURITY_INFORMATION,
        GetAce, GetAclInformation, GetFileSecurityW, INHERIT_ONLY_ACE, INHERITED_ACE,
        OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
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

const JOURNAL_VERSION: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const ACCESS_ALLOWED_ACE_TYPE_VALUE: u8 = 0;
const ACCESS_SYSTEM_SECURITY: u32 = 0x0100_0000;
const ROOT_SECURITY_INFORMATION: u32 =
    OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
const ROOT_DIRECTORY_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | FILE_DELETE_CHILD;
const DESCENDANT_DIRECTORY_ACCESS: u32 = ROOT_DIRECTORY_ACCESS | DELETE;
const DESCENDANT_FILE_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
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
        }
    }
}

/// Private R41.2 lease for one durable per-workspace DACL grant.
///
/// Provisioning records the exact pre-mutation root descriptor before applying the workspace SID.
/// Later runs only revalidate the durable grant and never rewrite the ACL. Public configuration
/// remains gated until explicit revalidate/remove actions and the full containment matrix pass.
pub(crate) struct WindowsFilesystemGrant {
    root: PathBuf,
    root_identity: RootIdentity,
    root_guard: File,
    lock: File,
    restricting_sid: WindowsRestrictingSid,
    lease_held: bool,
}

impl WindowsFilesystemGrant {
    pub(crate) fn acquire(root: &Path, state_dir: &Path) -> Result<Self> {
        assert_minimal_root_grant()?;
        let root = canonical_directory(root, "grant root")?;
        ensure_supported_local_ntfs(&root)?;
        let root_utf16 = wide_without_nul(root.as_os_str());
        let (root_guard, root_identity) = open_root_guard(&root)?;
        let state_dir = canonical_state_directory(state_dir, &root)?;
        let paths = GrantPaths::for_root(&state_dir, &root_utf16);
        let lock = open_and_lock(&paths.lock)?;
        let restricting_sid =
            provision_or_revalidate_locked(&root, &root_utf16, root_identity, &paths)?;

        Ok(Self {
            root,
            root_identity,
            root_guard,
            lock,
            restricting_sid,
            lease_held: true,
        })
    }

    pub(crate) fn restricting_sid(&self) -> &WindowsRestrictingSid {
        &self.restricting_sid
    }

    pub(crate) fn release(mut self) -> Result<()> {
        verify_open_root_identity(&self.root_guard, self.root_identity)?;
        verify_root_identity(&self.root, self.root_identity)?;
        verify_effective_root_grant(&self.root, &self.restricting_sid)?;
        FileExt::unlock(&self.lock).context("failed to release Windows ACL lease")?;
        self.lease_held = false;
        Ok(())
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

    #[cfg(test)]
    pub(crate) fn sid_has_security_control_rights(
        path: &Path,
        restricting_sid: &WindowsRestrictingSid,
    ) -> Result<bool> {
        let effective = effective_rights_for_path(path, restricting_sid)?;
        Ok(effective & FORBIDDEN_ROOT_GRANT_ACCESS != 0)
    }
}

impl Drop for WindowsFilesystemGrant {
    fn drop(&mut self) {
        if self.lease_held {
            let _ = FileExt::unlock(&self.lock);
        }
    }
}

fn provision_or_revalidate_locked(
    root: &Path,
    root_utf16: &[u16],
    root_identity: RootIdentity,
    paths: &GrantPaths,
) -> Result<WindowsRestrictingSid> {
    if !paths.journal.exists() {
        if paths.granted.exists() {
            bail!("Windows durable ACL marker exists without an owner journal");
        }
        if paths.snapshot.exists() {
            // Snapshot publication precedes journal publication, so a snapshot without a journal
            // proves that mutation was not yet allowed to begin.
            remove_if_exists(&paths.snapshot)?;
        }
        return provision_new_locked(root, root_utf16, root_identity, paths);
    }

    let journal = read_and_validate_journal(root_utf16, root_identity, paths)?;
    let restricting_sid = WindowsRestrictingSid::from_string(&journal.restricting_sid)?;
    verify_root_identity(root, root_identity)?;
    if durable_grant_marker_is_valid(paths)? {
        verify_effective_root_grant(root, &restricting_sid).context(
            "durable Windows workspace grant no longer matches its owner journal; explicit revalidation is required",
        )?;
        return Ok(restricting_sid);
    }

    // A durable prepared journal is the owner marker for an interrupted provisioning attempt.
    // Resume the exact owned grant instead of trying to reverse inheritance and normalize user
    // ACLs. The child cannot start until this grant is reread and the active marker is durable.
    match owned_root_grant_ace_count(root, &restricting_sid)? {
        0 => apply_and_verify_root_grant(root, &restricting_sid)
            .context("failed to resume durable Windows workspace grant provisioning")?,
        3 => verify_effective_root_grant(root, &restricting_sid)?,
        count => {
            bail!("durable Windows workspace grant has {count} owned root ACEs before activation")
        }
    }
    write_new_synced(&paths.granted, b"durable-grant-active\n")?;
    Ok(restricting_sid)
}

fn provision_new_locked(
    root: &Path,
    root_utf16: &[u16],
    root_identity: RootIdentity,
    paths: &GrantPaths,
) -> Result<WindowsRestrictingSid> {
    let restricting_sid = WindowsRestrictingSid::new_unique()?;
    let original = read_security_descriptor(root)?;
    let snapshot_sha256 = sha256_hex(original.as_bytes());
    write_new_synced(&paths.snapshot, original.as_bytes())?;
    let journal = GrantJournal {
        version: JOURNAL_VERSION,
        owner_marker: uuid::Uuid::new_v4().to_string(),
        root_utf16: root_utf16.to_vec(),
        root_identity,
        restricting_sid: restricting_sid.as_str().to_owned(),
        snapshot_sha256,
        snapshot_len: u64::try_from(original.len)
            .context("security descriptor length exceeds u64")?,
        phase: "durable-prepared".to_owned(),
    };
    let journal_bytes = serde_json::to_vec(&journal).context("failed to encode ACL journal")?;
    if let Err(error) = write_new_synced(&paths.journal, &journal_bytes) {
        let _ = remove_if_exists(&paths.snapshot);
        return Err(error);
    }

    // From this point on, failures retain the durable owner record. A later acquire can safely
    // resume the exact SID grant; attempting an automatic rollback would rewrite inherited child
    // ACLs and violate the no-normalization boundary proven by hosted Windows conformance.
    apply_and_verify_root_grant(root, &restricting_sid)
        .context("failed to provision durable Windows workspace grant; owner record retained")?;
    write_new_synced(&paths.granted, b"durable-grant-active\n").context(
        "durable Windows workspace grant is active but its marker was not published; owner record retained",
    )?;
    Ok(restricting_sid)
}

fn read_and_validate_journal(
    root_utf16: &[u16],
    root_identity: RootIdentity,
    paths: &GrantPaths,
) -> Result<GrantJournal> {
    let journal_bytes = fs::read(&paths.journal)
        .with_context(|| format!("failed to read {}", paths.journal.display()))?;
    let journal: GrantJournal = serde_json::from_slice(&journal_bytes)
        .with_context(|| format!("failed to parse {}", paths.journal.display()))?;
    if journal.version != JOURNAL_VERSION
        || journal.phase != "durable-prepared"
        || journal.root_utf16 != root_utf16
        || journal.root_identity != root_identity
        || journal.owner_marker.is_empty()
        || journal.restricting_sid.is_empty()
    {
        bail!("Windows durable ACL journal does not match the canonical root identity");
    }
    verify_snapshot_metadata(paths, &journal)?;
    Ok(journal)
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

fn durable_grant_marker_is_valid(paths: &GrantPaths) -> Result<bool> {
    match fs::read(&paths.granted) {
        Ok(marker) if marker == b"durable-grant-active\n" => Ok(true),
        Ok(_) => bail!("Windows durable ACL marker is corrupt"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to read {}", paths.granted.display()))
        }
    }
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

    let explicit = [
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: ROOT_DIRECTORY_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: trustee_for_sid(restricting_sid.as_ptr()),
        },
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: DESCENDANT_DIRECTORY_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE,
            Trustee: trustee_for_sid(restricting_sid.as_ptr()),
        },
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: DESCENDANT_FILE_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: OBJECT_INHERIT_ACE | INHERIT_ONLY_ACE,
            Trustee: trustee_for_sid(restricting_sid.as_ptr()),
        },
    ];
    let mut updated_dacl: *mut ACL = null_mut();
    // SAFETY: explicit and its SID remain live, current_dacl points into descriptor, and
    // updated_dacl is a valid output pointer.
    let merge_status = unsafe {
        SetEntriesInAclW(
            u32::try_from(explicit.len()).expect("owned grant ACE count fits u32"),
            explicit.as_ptr(),
            current_dacl,
            &raw mut updated_dacl,
        )
    };
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
    let owned_ace_count = owned_root_grant_ace_count(root, restricting_sid)?;
    if owned_ace_count != 3 {
        bail!(
            "Windows grant root must contain exactly three owned SID ACEs; found {owned_ace_count}"
        );
    }
    let effective = effective_rights_for_path(root, restricting_sid)?;
    if effective & ROOT_DIRECTORY_ACCESS != ROOT_DIRECTORY_ACCESS {
        bail!("Windows grant root did not expose the required minimal effective rights");
    }
    if effective & DELETE != 0 {
        bail!("Windows grant root exposed delete rights on the workspace root itself");
    }
    if effective & FORBIDDEN_ROOT_GRANT_ACCESS != 0 {
        bail!("Windows grant root exposed forbidden owner or ACL mutation rights");
    }
    Ok(())
}

fn owned_root_grant_ace_count(root: &Path, restricting_sid: &WindowsRestrictingSid) -> Result<u32> {
    let wide = nul_terminated(root.as_os_str(), "owned-grant path")?;
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: wide is NUL-terminated and all output pointers remain valid for the call.
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
            .context("failed to read Windows owned-grant DACL");
    }
    if dacl.is_null() {
        free_local(descriptor);
        bail!("Windows owned-grant root unexpectedly has a null DACL");
    }

    let mut information = ACL_SIZE_INFORMATION::default();
    // SAFETY: dacl points into the live descriptor and information is a valid output buffer.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut information).cast::<c_void>(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).expect("ACL size info fits u32"),
            AclSizeInformation,
        )
    } == 0
    {
        free_local(descriptor);
        return Err(io::Error::last_os_error())
            .context("failed to inspect Windows owned-grant ACL");
    }

    let expected = [
        (ROOT_DIRECTORY_ACCESS, 0_u8),
        (
            DESCENDANT_DIRECTORY_ACCESS,
            u8::try_from(CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE)
                .expect("Windows directory inheritance flags fit u8"),
        ),
        (
            DESCENDANT_FILE_ACCESS,
            u8::try_from(OBJECT_INHERIT_ACE | INHERIT_ONLY_ACE)
                .expect("Windows file inheritance flags fit u8"),
        ),
    ];
    let mut matching = [false; 3];
    for index in 0..information.AceCount {
        let mut raw_ace: *mut c_void = null_mut();
        // SAFETY: dacl is live, index is bounded by AceCount, and raw_ace is a valid output.
        if unsafe { GetAce(dacl, index, &raw mut raw_ace) } == 0 {
            free_local(descriptor);
            return Err(io::Error::last_os_error())
                .context("failed to enumerate Windows owned-grant ACE");
        }
        if raw_ace.is_null() {
            free_local(descriptor);
            bail!("GetAce returned a null Windows owned-grant ACE");
        }
        // SAFETY: GetAce returned a pointer to an ACE in the live ACL. Reading the common header
        // is valid for every ACE; ACCESS_ALLOWED_ACE fields are read only after checking AceType.
        let header = unsafe { &*raw_ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE_VALUE
            || u32::from(header.AceFlags) & INHERITED_ACE != 0
        {
            continue;
        }
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        let ace_sid = (&raw const ace.SidStart).cast_mut().cast::<c_void>();
        // SAFETY: ace_sid addresses the variable-length SID following ACCESS_ALLOWED_ACE and both
        // SID pointers remain live for the call.
        if unsafe { EqualSid(ace_sid, restricting_sid.as_ptr()) } == 0 {
            continue;
        }
        let Some(position) = expected
            .iter()
            .position(|candidate| *candidate == (ace.Mask, header.AceFlags))
        else {
            free_local(descriptor);
            bail!("Windows owned workspace SID ACE has an unexpected mask or inheritance flags");
        };
        if matching[position] {
            free_local(descriptor);
            bail!("Windows owned workspace SID ACE is duplicated");
        }
        matching[position] = true;
    }
    free_local(descriptor);
    Ok(
        u32::try_from(matching.into_iter().filter(|matched| *matched).count())
            .expect("owned grant ACE count fits u32"),
    )
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
    let file = open_root_identity_handle(
        root,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        "identity",
    )?;
    root_identity_from_handle(&file)
}

fn open_root_guard(root: &Path) -> Result<(File, RootIdentity)> {
    let file =
        open_root_identity_handle(root, FILE_SHARE_READ | FILE_SHARE_WRITE, "identity guard")?;
    let identity = root_identity_from_handle(&file)?;
    Ok((file, identity))
}

fn open_root_identity_handle(root: &Path, share_mode: u32, label: &str) -> Result<File> {
    let wide = nul_terminated(root.as_os_str(), "grant root")?;
    // SAFETY: wide is NUL-terminated. The returned handle is checked and then owned by File.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            share_mode,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("CreateFileW failed for Windows grant root {label}"));
    }
    // SAFETY: CreateFileW returned an owned, non-sentinel handle.
    Ok(unsafe { File::from_raw_handle(raw) })
}

fn root_identity_from_handle(file: &File) -> Result<RootIdentity> {
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

fn verify_open_root_identity(root_guard: &File, expected: RootIdentity) -> Result<()> {
    if root_identity_from_handle(root_guard)? != expected {
        bail!("Windows grant root handle identity changed during the ACL lease");
    }
    Ok(())
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

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn assert_minimal_root_grant() -> Result<()> {
    for access in [
        ROOT_DIRECTORY_ACCESS,
        DESCENDANT_DIRECTORY_ACCESS,
        DESCENDANT_FILE_ACCESS,
    ] {
        if access & FORBIDDEN_ROOT_GRANT_ACCESS != 0 {
            bail!("Windows restricted root grant contains forbidden security-control rights");
        }
    }
    if ROOT_DIRECTORY_ACCESS & DELETE != 0 {
        bail!("Windows restricted root grant permits deletion of the workspace root");
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
