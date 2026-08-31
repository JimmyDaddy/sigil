//! Platform process-birth observation.
//!
//! The values in this module are OS facts. They intentionally do not decide whether an observed
//! process may release a resource, terminate a tree, or complete a business operation. In
//! particular, an observation failure is never represented as process absence.

use std::fmt;

use sha2::{Digest, Sha256};

const BIRTH_IDENTITY_DOMAIN: &[u8] = b"sigil-process-birth-identity-v1\0";

/// A live process together with the platform facts that identify its birth.
///
/// The raw birth material remains private so callers cannot construct an identity from a PID or
/// a hash. Consumers can compare instances or use [`Self::birth_identity_fingerprint`] as an
/// opaque binding value.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessIdentityV1 {
    process_id: u32,
    birth: ProcessBirthIdentityV1,
}

impl fmt::Debug for ProcessIdentityV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessIdentityV1")
            .field("process_id", &self.process_id)
            .field("birth", &"<platform birth identity>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ProcessBirthIdentityV1 {
    #[cfg(target_os = "linux")]
    Linux {
        boot_id: String,
        observer_pid_namespace: String,
        target_pid_namespace: String,
        start_time_ticks: u64,
    },
    #[cfg(target_os = "macos")]
    MacOs {
        boot_session_uuid: String,
        start_seconds: u64,
        start_microseconds: u64,
    },
    #[cfg(windows)]
    Windows { creation_filetime: u64 },
}

impl ProcessIdentityV1 {
    /// Returns the operating-system process identifier observed with this birth identity.
    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Returns a stable opaque fingerprint of the complete platform birth material.
    ///
    /// This fingerprint is only a binding/comparison value. It does not upgrade a PID into a
    /// process proof, and the private raw material must still be re-observed before use.
    #[must_use]
    pub fn birth_identity_fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(BIRTH_IDENTITY_DOMAIN);
        hasher.update(self.process_id.to_be_bytes());
        match &self.birth {
            #[cfg(target_os = "linux")]
            ProcessBirthIdentityV1::Linux {
                boot_id,
                observer_pid_namespace,
                target_pid_namespace,
                start_time_ticks,
            } => {
                hasher.update(b"linux\0");
                update_sized_bytes(&mut hasher, boot_id.as_bytes());
                // The observer namespace explains how `/proc/<pid>` interpreted the numeric
                // PID; the target namespace binds the identity to the process that was read.
                update_sized_bytes(&mut hasher, observer_pid_namespace.as_bytes());
                update_sized_bytes(&mut hasher, target_pid_namespace.as_bytes());
                hasher.update(start_time_ticks.to_be_bytes());
            }
            #[cfg(target_os = "macos")]
            ProcessBirthIdentityV1::MacOs {
                boot_session_uuid,
                start_seconds,
                start_microseconds,
            } => {
                hasher.update(b"macos\0");
                update_sized_bytes(&mut hasher, boot_session_uuid.as_bytes());
                hasher.update(start_seconds.to_be_bytes());
                hasher.update(start_microseconds.to_be_bytes());
            }
            #[cfg(windows)]
            ProcessBirthIdentityV1::Windows { creation_filetime } => {
                hasher.update(b"windows\0");
                hasher.update(creation_filetime.to_be_bytes());
            }
        }
        hasher.finalize().into()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn update_sized_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// A platform observation failure that cannot be converted into an absence or terminal fact.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessIdentityObservationErrorV1 {
    #[error("process identifier is invalid")]
    InvalidProcessId,
    #[error("process is absent")]
    Absent,
    #[error("process is no longer live: {0}")]
    NotLive(String),
    #[error("process birth identity is not observable: {0}")]
    NotObservable(String),
}

/// Observes the current host process with real platform birth facts.
///
/// # Errors
///
/// Returns a typed observation failure when the platform cannot supply a complete birth
/// identity. The current process is never treated as absent on such a failure.
pub fn observe_current_process_identity()
-> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    observe_process_identity(std::process::id())
}

/// Observes one currently live process using the platform's birth discriminator.
///
/// A successful result only means that a matching live process was observed. A caller that needs
/// to act on the process must keep and revalidate the exact identity; it must not reuse the PID.
///
/// # Errors
///
/// Returns [`ProcessIdentityObservationErrorV1::Absent`] only when the platform reports this
/// exact PID as missing. Permission, short-read, parse, namespace, handle, and other system
/// failures return [`ProcessIdentityObservationErrorV1::NotObservable`]. A platform-confirmed
/// exited or zombie process returns [`ProcessIdentityObservationErrorV1::NotLive`].
pub fn observe_process_identity(
    process_id: u32,
) -> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    if process_id == 0 {
        return Err(ProcessIdentityObservationErrorV1::InvalidProcessId);
    }
    observe_process_identity_platform(process_id)
}

#[cfg(target_os = "linux")]
fn observe_process_identity_platform(
    process_id: u32,
) -> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    use std::path::Path;

    const MAX_PROC_TEXT_BYTES: usize = 16 * 1024;

    if i32::try_from(process_id).is_err() {
        return Err(ProcessIdentityObservationErrorV1::InvalidProcessId);
    }

    let boot_before = read_limited_proc_text(
        Path::new("/proc/sys/kernel/random/boot_id"),
        MAX_PROC_TEXT_BYTES,
    )
    .map_err(|error| map_linux_platform_error(error, "Linux boot identity"))?;
    let observer_pid_namespace_before = linux_observer_pid_namespace()?;
    let target_pid_namespace_before = linux_target_pid_namespace(process_id)?;
    let stat_path = format!("/proc/{process_id}/stat");
    let stat = read_limited_proc_text(Path::new(&stat_path), MAX_PROC_TEXT_BYTES)
        .map_err(|error| map_linux_process_error(error, "Linux process stat"))?;
    let stat = parse_linux_process_stat(&stat, process_id)?;
    ensure_linux_process_is_live(stat.state)?;
    let stat_after = read_limited_proc_text(Path::new(&stat_path), MAX_PROC_TEXT_BYTES)
        .map_err(|error| map_linux_process_error(error, "Linux process stat"))?;
    let stat_after = parse_linux_process_stat(&stat_after, process_id)?;
    ensure_linux_process_is_live(stat_after.state)?;
    let boot_after = read_limited_proc_text(
        Path::new("/proc/sys/kernel/random/boot_id"),
        MAX_PROC_TEXT_BYTES,
    )
    .map_err(|error| map_linux_platform_error(error, "Linux boot identity"))?;
    let observer_pid_namespace_after = linux_observer_pid_namespace()?;
    let target_pid_namespace_after = linux_target_pid_namespace(process_id)?;

    if boot_before != boot_after
        || observer_pid_namespace_before != observer_pid_namespace_after
        || target_pid_namespace_before != target_pid_namespace_after
        || stat != stat_after
    {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Linux boot, observer PID namespace, or target PID namespace changed during process observation"
                .to_owned(),
        ));
    }

    Ok(ProcessIdentityV1 {
        process_id,
        birth: ProcessBirthIdentityV1::Linux {
            boot_id: boot_before,
            observer_pid_namespace: observer_pid_namespace_before,
            target_pid_namespace: target_pid_namespace_before,
            start_time_ticks: stat.start_time_ticks,
        },
    })
}

#[cfg(target_os = "linux")]
fn linux_observer_pid_namespace() -> Result<String, ProcessIdentityObservationErrorV1> {
    linux_pid_namespace(std::path::Path::new("/proc/self/ns/pid"), None)
}

#[cfg(target_os = "linux")]
fn linux_target_pid_namespace(
    process_id: u32,
) -> Result<String, ProcessIdentityObservationErrorV1> {
    let path = format!("/proc/{process_id}/ns/pid");
    linux_pid_namespace(std::path::Path::new(&path), Some(process_id))
}

#[cfg(target_os = "linux")]
fn linux_pid_namespace(
    path: &std::path::Path,
    target_process_id: Option<u32>,
) -> Result<String, ProcessIdentityObservationErrorV1> {
    use std::os::unix::ffi::OsStrExt;

    let namespace = std::fs::read_link(path).map_err(|error| match target_process_id {
        Some(_) => map_linux_process_error(error, "Linux target PID namespace"),
        None => map_linux_platform_error(error, "Linux observer PID namespace"),
    })?;
    std::str::from_utf8(namespace.as_os_str().as_bytes())
        .map(|value| value.to_owned())
        .map_err(|_| {
            ProcessIdentityObservationErrorV1::NotObservable(
                "Linux PID namespace is not valid UTF-8".to_owned(),
            )
        })
}

#[cfg(target_os = "linux")]
fn read_limited_proc_text(path: &std::path::Path, maximum: usize) -> std::io::Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(maximum.min(1024));
    file.by_ref()
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(std::io::Error::other(
            "proc file exceeds bounded observation size",
        ));
    }
    let value = std::str::from_utf8(&bytes).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "proc file is not UTF-8")
    })?;
    Ok(value.trim_end().to_owned())
}

#[cfg(target_os = "linux")]
fn map_linux_process_error(
    error: std::io::Error,
    subject: &str,
) -> ProcessIdentityObservationErrorV1 {
    if error.kind() == std::io::ErrorKind::NotFound {
        ProcessIdentityObservationErrorV1::Absent
    } else {
        ProcessIdentityObservationErrorV1::NotObservable(format!("{subject}: {error}"))
    }
}

#[cfg(target_os = "linux")]
fn map_linux_platform_error(
    error: std::io::Error,
    subject: &str,
) -> ProcessIdentityObservationErrorV1 {
    ProcessIdentityObservationErrorV1::NotObservable(format!("{subject}: {error}"))
}

#[cfg(target_os = "linux")]
#[derive(PartialEq, Eq)]
struct LinuxProcessStatV1 {
    state: char,
    start_time_ticks: u64,
}

#[cfg(target_os = "linux")]
fn parse_linux_process_stat(
    stat: &str,
    process_id: u32,
) -> Result<LinuxProcessStatV1, ProcessIdentityObservationErrorV1> {
    let open = stat.find('(').ok_or_else(|| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat is missing the command delimiter".to_owned(),
        )
    })?;
    let reported_process_id = stat[..open].trim().parse::<u32>().map_err(|_| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat has an invalid process identifier".to_owned(),
        )
    })?;
    if reported_process_id != process_id {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat process identifier drifted".to_owned(),
        ));
    }

    // `comm` is parenthesized and may itself contain spaces or closing parentheses. The final
    // closing parenthesis is the only delimiter whose suffix begins the fixed field sequence.
    let close = stat.rfind(')').ok_or_else(|| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat is missing the closing command delimiter".to_owned(),
        )
    })?;
    let suffix = stat.get(close + 1..).ok_or_else(|| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat command delimiter is truncated".to_owned(),
        )
    })?;
    let fields = suffix.split_ascii_whitespace().collect::<Vec<_>>();
    // The suffix starts at field 3 (`state`), so index 19 is field 22 (`starttime`).
    let state = fields.first().and_then(|state| {
        let mut bytes = state.bytes();
        let state = bytes.next()?;
        (bytes.next().is_none() && state.is_ascii()).then_some(char::from(state))
    });
    let state = state.ok_or_else(|| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat state is invalid".to_owned(),
        )
    })?;
    let start_time = fields.get(19).ok_or_else(|| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat is truncated before start time".to_owned(),
        )
    })?;
    let start_time_ticks = start_time.parse::<u64>().map_err(|_| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Linux process stat start time is invalid".to_owned(),
        )
    })?;
    Ok(LinuxProcessStatV1 {
        state,
        start_time_ticks,
    })
}

#[cfg(target_os = "linux")]
fn ensure_linux_process_is_live(state: char) -> Result<(), ProcessIdentityObservationErrorV1> {
    match state {
        // Linux task state letters documented for `/proc/<pid>/stat`. A stopped task is still a
        // live process birth; it must not be mistaken for terminal evidence.
        'R' | 'S' | 'D' | 'T' | 't' | 'W' | 'I' | 'P' => Ok(()),
        'Z' | 'X' | 'x' => Err(ProcessIdentityObservationErrorV1::NotLive(format!(
            "Linux process state {state:?}"
        ))),
        _ => Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Linux process state {state:?} is not recognized"
        ))),
    }
}

#[cfg(target_os = "macos")]
fn observe_process_identity_platform(
    process_id: u32,
) -> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    use std::{io, mem::size_of};

    let process_id = i32::try_from(process_id)
        .map_err(|_| ProcessIdentityObservationErrorV1::InvalidProcessId)?;
    let boot_before = macos_boot_session_uuid()?;
    // SAFETY: `proc_bsdinfo` is a C-compatible output structure. Zero initialization is valid
    // for every field before `proc_pidinfo` writes its returned byte count.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is aligned writable storage for exactly `proc_bsdinfo`; the public Darwin
    // ABI takes a borrowed buffer and does not retain it beyond this call.
    let returned = unsafe {
        libc::proc_pidinfo(
            process_id,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if returned != size_of::<libc::proc_bsdinfo>() as i32 {
        let error = io::Error::last_os_error();
        if returned == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            return Err(ProcessIdentityObservationErrorV1::Absent);
        }
        return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Darwin proc_pidinfo returned {returned} bytes: {error}"
        )));
    }
    if info.pbi_pid != process_id as u32 {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin proc_pidinfo process identifier drifted".to_owned(),
        ));
    }
    ensure_macos_process_is_live(info.pbi_status)?;
    // Read the same PID again before returning its birth identity. `proc_pidinfo` is a query,
    // not a process handle; matching the immutable start timestamp closes a PID-reuse window
    // inside this bounded observation.
    // SAFETY: `proc_bsdinfo` is a C-compatible output structure. Zero initialization is valid
    // for every field before `proc_pidinfo` writes its returned byte count.
    let mut info_after: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info_after` is aligned writable storage for exactly `proc_bsdinfo`; the public
    // Darwin ABI takes a borrowed buffer and does not retain it beyond this call.
    let returned_after = unsafe {
        libc::proc_pidinfo(
            process_id,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info_after).cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if returned_after != size_of::<libc::proc_bsdinfo>() as i32 {
        let error = io::Error::last_os_error();
        if returned_after == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            return Err(ProcessIdentityObservationErrorV1::Absent);
        }
        return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Darwin proc_pidinfo recheck returned {returned_after} bytes: {error}"
        )));
    }
    if info_after.pbi_pid != process_id as u32
        || info_after.pbi_start_tvsec != info.pbi_start_tvsec
        || info_after.pbi_start_tvusec != info.pbi_start_tvusec
    {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin process identifier or birth time changed during process observation".to_owned(),
        ));
    }
    ensure_macos_process_is_live(info_after.pbi_status)?;
    let boot_after = macos_boot_session_uuid()?;
    if boot_before != boot_after {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin boot session changed during process observation".to_owned(),
        ));
    }

    Ok(ProcessIdentityV1 {
        process_id: process_id as u32,
        birth: ProcessBirthIdentityV1::MacOs {
            boot_session_uuid: boot_before,
            start_seconds: info.pbi_start_tvsec,
            start_microseconds: info.pbi_start_tvusec,
        },
    })
}

#[cfg(target_os = "macos")]
fn ensure_macos_process_is_live(status: u32) -> Result<(), ProcessIdentityObservationErrorV1> {
    // `proc_bsdinfo::pbi_status` carries the BSD `p_stat` values published by XNU's
    // `proc_info.h`: SIDL=1, SRUN=2, SSLEEP=3, SSTOP=4, and SZOMB=5.
    match status {
        2..=4 => Ok(()),
        1 | 5 => Err(ProcessIdentityObservationErrorV1::NotLive(format!(
            "Darwin process status {status}"
        ))),
        _ => Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Darwin process status {status} is not recognized"
        ))),
    }
}

#[cfg(target_os = "macos")]
fn macos_boot_session_uuid() -> Result<String, ProcessIdentityObservationErrorV1> {
    use std::io;

    const NAME: &[u8] = b"kern.bootsessionuuid\0";
    let mut bytes = [0u8; 64];
    let mut length = bytes.len();
    // SAFETY: NAME is NUL-terminated, bytes is writable for `length` bytes, and this call does
    // not retain either pointer. `kern.bootsessionuuid` is a read-only public sysctl.
    let result = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length == 0 || length > bytes.len() {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Darwin boot session UUID is unavailable: {}",
            io::Error::last_os_error()
        )));
    }
    let value = &bytes[..length];
    let value = value.strip_suffix(&[0]).unwrap_or(value);
    if value.contains(&0) {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin boot session UUID contains an interior NUL".to_owned(),
        ));
    }
    let uuid = std::str::from_utf8(value).map_err(|_| {
        ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin boot session UUID is not valid UTF-8".to_owned(),
        )
    })?;
    if !is_macos_boot_session_uuid(uuid) {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Darwin boot session UUID has an invalid format".to_owned(),
        ));
    }
    Ok(uuid.to_owned())
}

#[cfg(target_os = "macos")]
fn is_macos_boot_session_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
        })
}

#[cfg(windows)]
fn observe_process_identity_platform(
    process_id: u32,
) -> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    use std::{
        io,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };

    use windows_sys::Win32::{
        Foundation::{FILETIME, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Storage::FileSystem::SYNCHRONIZE,
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
        },
    };

    // SAFETY: OpenProcess receives a scalar PID, documented query and synchronize access rights,
    // and a false inheritance flag. The returned handle is checked before ownership transfer.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            process_id,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(87) {
            return Err(ProcessIdentityObservationErrorV1::Absent);
        }
        return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Windows process handle is unavailable: {error}"
        )));
    }
    // SAFETY: `raw` is a fresh non-null process handle owned by this function and is transferred
    // exactly once to OwnedHandle for CloseHandle-on-drop.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    // SAFETY: `handle` stays open for this zero-timeout wait. A process handle becomes signaled
    // only after the process terminates, and the call does not modify process state.
    match unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) } {
        WAIT_TIMEOUT => {}
        WAIT_OBJECT_0 => {
            return Err(ProcessIdentityObservationErrorV1::NotLive(
                "Windows process handle is signaled".to_owned(),
            ));
        }
        WAIT_FAILED => {
            return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
                "Windows process wait failed: {}",
                io::Error::last_os_error()
            )));
        }
        result => {
            return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
                "Windows process wait returned unexpected result {result}"
            )));
        }
    }
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all FILETIME pointers are valid writable storage, and OwnedHandle keeps the exact
    // process handle open for the duration of GetProcessTimes.
    let read = unsafe {
        GetProcessTimes(
            handle.as_raw_handle().cast(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if read == 0 {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(format!(
            "Windows process creation time is unavailable: {}",
            io::Error::last_os_error()
        )));
    }
    let creation_filetime =
        ((creation.dwHighDateTime as u64) << 32) | u64::from(creation.dwLowDateTime);
    if creation_filetime == 0 {
        return Err(ProcessIdentityObservationErrorV1::NotObservable(
            "Windows process creation time is zero".to_owned(),
        ));
    }
    // Microsoft documents the exit FILETIME as undefined while a process is running. It is an
    // API-required output buffer only; liveness comes solely from the zero-timeout wait above.
    Ok(ProcessIdentityV1 {
        process_id,
        birth: ProcessBirthIdentityV1::Windows { creation_filetime },
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn observe_process_identity_platform(
    _process_id: u32,
) -> Result<ProcessIdentityV1, ProcessIdentityObservationErrorV1> {
    Err(ProcessIdentityObservationErrorV1::NotObservable(
        "this platform has no real process birth identity implementation".to_owned(),
    ))
}

#[cfg(test)]
#[path = "tests/identity_tests.rs"]
mod tests;
