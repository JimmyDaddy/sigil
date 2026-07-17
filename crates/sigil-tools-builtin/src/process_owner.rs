#[cfg(windows)]
mod windows {
    use std::{
        collections::BTreeMap,
        ffi::c_void,
        io,
        mem::{size_of, zeroed},
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
            // SAFETY: both optional pointers are null, and the returned handle is checked before
            // it is transferred into `OwnedHandle`.
            let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error())
                    .context("failed to create Windows Job Object");
            }
            // SAFETY: `raw` is a fresh, non-null owned Job Object handle.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            // SAFETY: the Win32 structure is plain data and its documented zero state is valid.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the handle remains owned by `handle`; `limits` points to a correctly sized
            // structure for `JobObjectExtendedLimitInformation` for the duration of the call.
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
            // SAFETY: `OpenProcess` receives a concrete child pid and only the rights required for
            // Job Object assignment/termination. The result is checked before ownership transfer.
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
            // SAFETY: `process` is a fresh, non-null owned process handle.
            let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
            // SAFETY: both handles remain valid for the duration of the call.
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
            // SAFETY: the Job Object handle is owned by `self` and stays valid through the call.
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

    pub(crate) struct ProcessTreeOwnerGuard {
        process_id: u32,
    }

    impl ProcessTreeOwnerGuard {
        pub(crate) fn assign(process_id: Option<u32>) -> Result<Self> {
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
    }

    impl Drop for ProcessTreeOwnerGuard {
        fn drop(&mut self) {
            if let Ok(mut registry) = registry().lock() {
                registry.remove(&self.process_id);
            }
        }
    }

    pub(crate) fn terminate_owned_process_tree(process_id: u32) -> Result<()> {
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

    pub(crate) fn validate_process_tree_owner() -> Result<()> {
        drop(WindowsJob::create()?);
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) use windows::ProcessTreeOwnerGuard;
#[cfg(windows)]
pub(crate) use windows::terminate_owned_process_tree;
#[cfg(windows)]
pub(crate) use windows::validate_process_tree_owner;

#[cfg(not(windows))]
pub(crate) struct ProcessTreeOwnerGuard;

#[cfg(not(windows))]
impl ProcessTreeOwnerGuard {
    pub(crate) fn assign(_process_id: Option<u32>) -> anyhow::Result<Self> {
        Ok(Self)
    }
}

#[cfg(not(windows))]
pub(crate) fn validate_process_tree_owner() -> anyhow::Result<()> {
    Ok(())
}
