//! Cross-crate process-tree lifecycle ownership.
//!
//! This crate intentionally owns only process admission into an operating-system lifecycle group,
//! group termination, and an offline capability probe. Shell selection, sandbox policy, terminal
//! I/O, MCP framing, desktop bootstrap, and product receipts remain in their caller crates.

use std::process::Command;

/// Configures a child command to become the root of an owned process tree.
///
/// Unix uses a new process group. Windows attaches the concrete process to a Job Object after
/// spawn through [`ProcessTreeOwnerGuard::assign`].
pub fn configure_process_tree(command: &mut Command) {
    configure_process_tree_platform(command);
}

#[cfg(unix)]
fn configure_process_tree_platform(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_tree_platform(_command: &mut Command) {}

#[cfg(windows)]
mod windows {
    // Rust's standard library does not expose Windows Job Object lifecycle APIs. Keep the
    // unavoidable unsafe surface limited to direct Win32 calls and raw-handle ownership transfer.
    use std::{
        collections::BTreeMap,
        ffi::c_void,
        io,
        mem::size_of,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr,
        sync::{Arc, Mutex, OnceLock},
    };

    use anyhow::{Context, Result, anyhow, bail};
    use windows_sys::Win32::System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        },
    };

    struct WindowsJob {
        handle: OwnedHandle,
    }

    impl WindowsJob {
        fn create() -> Result<Self> {
            // SAFETY: Win32 permits null security-attribute and name pointers here. No Rust
            // references cross the FFI boundary, and the returned handle is checked before use.
            let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error())
                    .context("failed to create Windows Job Object");
            }
            // SAFETY: `raw` is a fresh, non-null Job Object handle that is closed with
            // `CloseHandle`; ownership is transferred exactly once to `OwnedHandle`.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `handle` keeps the Job Object valid. `limits` is the matching repr(C) Win32
            // structure, and its aligned pointer remains readable for the full call with the exact
            // structure size supplied.
            let configured = unsafe {
                SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to configure Windows Job Object kill-on-close");
            }
            Ok(Self { handle })
        }

        fn assign_process(&self, process_id: u32) -> Result<()> {
            // SAFETY: the FFI call receives an integer pid, valid access flags, and a false handle
            // inheritance flag. The returned handle is checked before any use or ownership transfer.
            let process = unsafe {
                OpenProcess(
                    PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    process_id,
                )
            };
            if process.is_null() {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("failed to open Windows child process {process_id}"));
            }
            // SAFETY: `process` is a fresh, non-null process handle that is closed with
            // `CloseHandle`; ownership is transferred exactly once to `OwnedHandle`.
            let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
            // SAFETY: both `OwnedHandle` values keep their raw handles valid for the full call;
            // `AssignProcessToJobObject` only borrows them and does not take ownership.
            let assigned = unsafe {
                AssignProcessToJobObject(
                    self.handle.as_raw_handle().cast::<c_void>(),
                    process.as_raw_handle().cast::<c_void>(),
                )
            };
            if assigned == 0 {
                return Err(io::Error::last_os_error()).with_context(|| {
                    format!("failed to assign Windows child process {process_id} to Job Object")
                });
            }
            Ok(())
        }

        fn terminate(&self) -> Result<()> {
            // SAFETY: `self.handle` keeps the Job Object handle valid for the full call;
            // `TerminateJobObject` borrows the handle and does not take ownership.
            let terminated =
                unsafe { TerminateJobObject(self.handle.as_raw_handle().cast::<c_void>(), 1) };
            if terminated == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to terminate Windows Job Object");
            }
            Ok(())
        }
    }

    fn registry() -> &'static Mutex<BTreeMap<u32, Arc<WindowsJob>>> {
        static REGISTRY: OnceLock<Mutex<BTreeMap<u32, Arc<WindowsJob>>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    /// Keeps one child and its descendants bound to a kill-on-close Job Object.
    pub struct ProcessTreeOwnerGuard {
        process_id: u32,
    }

    impl ProcessTreeOwnerGuard {
        /// Creates a Job Object and immediately assigns the concrete child process.
        ///
        /// # Errors
        ///
        /// Returns an error when the child identity is absent, a Job Object cannot be created, or
        /// Windows rejects assignment of the child to the new owner.
        pub fn assign(process_id: Option<u32>) -> Result<Self> {
            let process_id =
                process_id.ok_or_else(|| anyhow!("Windows child process id unavailable"))?;
            let mut registry = registry()
                .lock()
                .map_err(|_| anyhow!("Windows Job Object registry lock poisoned"))?;
            if registry.contains_key(&process_id) {
                bail!("Windows Job Object registry already owned process {process_id}");
            }
            let job = Arc::new(WindowsJob::create()?);
            job.assign_process(process_id)?;
            registry.insert(process_id, job);
            Ok(Self { process_id })
        }

        /// Terminates the exact Job Object owned by this guard.
        ///
        /// # Errors
        ///
        /// Returns an error when the owner registration is unavailable or Windows rejects Job
        /// termination.
        pub fn terminate(&self) -> Result<()> {
            terminate_owned_process_tree(self.process_id)
        }
    }

    impl Drop for ProcessTreeOwnerGuard {
        fn drop(&mut self) {
            if let Ok(mut registry) = registry().lock() {
                registry.remove(&self.process_id);
            }
        }
    }

    /// Terminates the Job Object currently owning `process_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when no live owner is registered or Windows rejects termination.
    pub fn terminate_owned_process_tree(process_id: u32) -> Result<()> {
        let job = registry()
            .lock()
            .map_err(|_| anyhow!("Windows Job Object registry lock poisoned"))?
            .get(&process_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!("Windows Job Object owner is unavailable for process {process_id}")
            })?;
        job.terminate()
    }

    /// Verifies that a kill-on-close Job Object can be created on the current host.
    ///
    /// # Errors
    ///
    /// Returns an error when Windows cannot create or configure the Job Object.
    pub fn validate_process_tree_owner() -> Result<()> {
        drop(WindowsJob::create()?);
        Ok(())
    }
}

#[cfg(windows)]
pub use windows::{
    ProcessTreeOwnerGuard, terminate_owned_process_tree, validate_process_tree_owner,
};

/// Terminates the Unix process group rooted at `process_id`.
///
/// A missing group is already quiescent and is therefore accepted. Callers still own waiting for
/// and reaping their direct child.
///
/// # Errors
///
/// Returns an error when the process id cannot be represented by the platform or the operating
/// system rejects the group signal.
#[cfg(unix)]
pub fn terminate_owned_process_tree(process_id: u32) -> anyhow::Result<()> {
    use anyhow::{Context, anyhow};
    use nix::{errno::Errno, sys::signal, unistd::Pid};

    let process_id = i32::try_from(process_id)
        .map_err(|_| anyhow!("child process id is outside the Unix pid range"))?;
    match signal::killpg(Pid::from_raw(process_id), signal::Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error).context("failed to terminate owned Unix process group"),
    }
}

/// Process-group identity guard for non-Windows targets.
#[cfg(not(windows))]
pub struct ProcessTreeOwnerGuard {
    process_id: Option<u32>,
}

#[cfg(not(windows))]
impl ProcessTreeOwnerGuard {
    /// Accepts the child identity while leaving Unix process-group semantics to the caller.
    ///
    /// # Errors
    ///
    /// The non-Windows implementation currently cannot fail.
    pub fn assign(process_id: Option<u32>) -> anyhow::Result<Self> {
        Ok(Self { process_id })
    }

    /// Terminates the exact process group bound to this guard.
    ///
    /// # Errors
    ///
    /// Returns an error when the child identity was unavailable or the platform rejects process-
    /// tree termination.
    pub fn terminate(&self) -> anyhow::Result<()> {
        let process_id = self
            .process_id
            .ok_or_else(|| anyhow::anyhow!("owned child process id is unavailable"))?;
        terminate_owned_process_tree(process_id)
    }
}

/// The non-Windows capability probe is a no-op because process groups use standard Unix spawn and
/// signal primitives.
///
/// # Errors
///
/// The non-Windows implementation currently cannot fail.
#[cfg(not(windows))]
pub fn validate_process_tree_owner() -> anyhow::Result<()> {
    Ok(())
}

/// Reports that process-tree termination is unavailable on unsupported non-Unix targets.
///
/// # Errors
///
/// Always returns an error because no platform ownership primitive is implemented.
#[cfg(not(any(unix, windows)))]
pub fn terminate_owned_process_tree(_process_id: u32) -> anyhow::Result<()> {
    anyhow::bail!("process-tree termination is unsupported on this platform")
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
