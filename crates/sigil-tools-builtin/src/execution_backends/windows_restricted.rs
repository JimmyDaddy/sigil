use anyhow::Result;
use sigil_kernel::{
    ExecutionOutputReceipt, ExecutionRequest, ExecutionResourceReceipt, ProcessEnvironmentPolicy,
};

#[cfg(windows)]
use super::{
    OutputCollectionLimits, PreflightReaderFault, SupervisedExecutionChild, execution_deadline,
    supervise_execution_child,
};

/// Evidence returned by the private R41.1 restricted-launch probe.
///
/// This is intentionally not an `ExecutionReceipt`: Windows restricted execution is not a
/// selectable public backend until filesystem containment is proven by R41.2.
#[derive(Debug)]
pub(crate) struct WindowsRestrictedProbeReceipt {
    pub(crate) privileges_constrained: bool,
    pub(crate) source_enabled_non_traverse_privilege_count: u32,
    pub(crate) restricted_enabled_non_traverse_privilege_count: u32,
    pub(crate) restricting_sid_count: u32,
    pub(crate) environment_policy: ProcessEnvironmentPolicy,
    pub(crate) resources: ExecutionResourceReceipt,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) output: ExecutionOutputReceipt,
    pub(crate) timed_out: bool,
}

/// Typed platform failure for the private probe on non-Windows hosts.
#[derive(Debug, thiserror::Error)]
#[error("Windows restricted launch probe is unavailable on {platform}")]
pub(crate) struct WindowsRestrictedProbeUnavailable {
    platform: &'static str,
}

impl WindowsRestrictedProbeUnavailable {
    #[must_use]
    pub(crate) fn platform(&self) -> &'static str {
        self.platform
    }
}

#[cfg(not(windows))]
pub(crate) async fn windows_restricted_launch_probe(
    _request: &ExecutionRequest,
) -> Result<WindowsRestrictedProbeReceipt> {
    Err(anyhow::Error::new(WindowsRestrictedProbeUnavailable {
        platform: std::env::consts::OS,
    }))
}

#[cfg(windows)]
pub(crate) async fn windows_restricted_launch_probe(
    request: &ExecutionRequest,
) -> Result<WindowsRestrictedProbeReceipt> {
    let child = NativeWindowsRestrictedChild::spawn(request)?;
    let privilege_evidence = child.privilege_evidence();
    let deadline = execution_deadline(request)?;
    let outcome = supervise_execution_child(
        SupervisedExecutionChild::WindowsRestricted(child),
        request,
        OutputCollectionLimits::execution(),
        PreflightReaderFault::None,
        None,
        deadline,
        None,
    )
    .await?;
    Ok(WindowsRestrictedProbeReceipt {
        privileges_constrained: privilege_evidence.privileges_constrained(),
        source_enabled_non_traverse_privilege_count: privilege_evidence
            .source_enabled_non_traverse_privilege_count,
        restricted_enabled_non_traverse_privilege_count: privilege_evidence
            .restricted_enabled_non_traverse_privilege_count,
        restricting_sid_count: privilege_evidence.restricting_sid_count,
        environment_policy: request.environment_policy,
        resources: outcome.resources,
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        output: outcome.output,
        timed_out: outcome.timed_out,
    })
}

#[cfg(windows)]
mod native {
    use std::{
        cmp::Ordering,
        ffi::{OsStr, OsString, c_void},
        fs, io,
        mem::{size_of, size_of_val},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::ExitStatusExt,
        },
        path::Path,
        process::ExitStatus,
        ptr::{null, null_mut},
        sync::Arc,
    };

    use anyhow::{Context, Result, bail};
    use sigil_kernel::{ExecutionRequest, ProcessEnvironmentPolicy};
    use tokio::{fs::File as TokioFile, task::JoinHandle};
    use windows_sys::Win32::{
        Foundation::{
            HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
            WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
        Security::{
            Authorization::ConvertStringSidToSidW, CreateRestrictedToken, DISABLE_MAX_PRIVILEGE,
            GetTokenInformation, LookupPrivilegeValueW, PSID, SE_CHANGE_NOTIFY_NAME,
            SE_PRIVILEGE_ENABLED, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES, TOKEN_ASSIGN_PRIMARY,
            TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenPrivileges,
            TokenRestrictedSids, WRITE_RESTRICTED,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
                CreateProcessAsUserW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
                OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
                ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    };

    const TERMINATED_BY_SUPERVISOR_EXIT_CODE: u32 = 1;

    #[derive(Clone, Copy)]
    pub(crate) struct RestrictedTokenPrivilegeEvidence {
        pub(crate) source_enabled_non_traverse_privilege_count: u32,
        pub(crate) restricted_enabled_non_traverse_privilege_count: u32,
        pub(crate) restricting_sid_count: u32,
    }

    pub(crate) struct WindowsRestrictingSid {
        sid: PSID,
    }

    impl WindowsRestrictingSid {
        pub(crate) fn new_unique() -> Result<Self> {
            let bytes = *uuid::Uuid::new_v4().as_bytes();
            let components = bytes
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("UUID chunk is 4 bytes")))
                .collect::<Vec<_>>();
            Self::from_string(&format!(
                "S-1-5-21-{}-{}-{}-{}",
                components[0], components[1], components[2], components[3]
            ))
        }

        fn from_string(value: &str) -> Result<Self> {
            let wide = nul_terminated(OsStr::new(value), "restricting SID")?;
            let mut sid: PSID = null_mut();
            // SAFETY: wide is NUL-terminated and sid is a valid output pointer. The returned SID
            // is released with LocalFree in Drop.
            if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut sid) } == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to parse Windows restricting SID");
            }
            if sid.is_null() {
                bail!("ConvertStringSidToSidW returned a null restricting SID");
            }
            Ok(Self { sid })
        }

        pub(crate) fn as_ptr(&self) -> PSID {
            self.sid
        }
    }

    impl Drop for WindowsRestrictingSid {
        fn drop(&mut self) {
            if !self.sid.is_null() {
                // SAFETY: ConvertStringSidToSidW allocated this SID with LocalAlloc.
                let _ = unsafe { LocalFree(self.sid) };
            }
        }
    }

    impl RestrictedTokenPrivilegeEvidence {
        pub(crate) fn privileges_constrained(self) -> bool {
            self.restricted_enabled_non_traverse_privilege_count == 0
        }
    }

    pub(crate) struct NativeWindowsRestrictedChild {
        process: Arc<OwnedHandle>,
        thread: Option<OwnedHandle>,
        process_id: u32,
        stdout: Option<TokioFile>,
        stderr: Option<TokioFile>,
        wait_task: Option<JoinHandle<io::Result<ExitStatus>>>,
        status: Option<ExitStatus>,
        privilege_evidence: RestrictedTokenPrivilegeEvidence,
    }

    impl NativeWindowsRestrictedChild {
        pub(super) fn spawn(request: &ExecutionRequest) -> Result<Self> {
            Self::spawn_with_optional_restricting_sid(request, None)
        }

        pub(crate) fn spawn_with_restricting_sid(
            request: &ExecutionRequest,
            restricting_sid: &WindowsRestrictingSid,
        ) -> Result<Self> {
            Self::spawn_with_optional_restricting_sid(request, Some(restricting_sid))
        }

        fn spawn_with_optional_restricting_sid(
            request: &ExecutionRequest,
            restricting_sid: Option<&WindowsRestrictingSid>,
        ) -> Result<Self> {
            let program = canonical_file(Path::new(&request.program), "program")?;
            let cwd = fs::canonicalize(&request.cwd).with_context(|| {
                format!(
                    "failed to canonicalize Windows restricted launch cwd {}",
                    request.cwd.display()
                )
            })?;
            if !cwd.is_dir() {
                bail!(
                    "Windows restricted launch cwd is not a directory: {}",
                    cwd.display()
                );
            }

            let process_token = open_current_process_token()?;
            let restricted_token =
                create_restricted_token(raw_handle(&process_token), restricting_sid)?;
            let privilege_evidence = restricted_token_privilege_evidence(
                raw_handle(&process_token),
                raw_handle(&restricted_token),
            )?;
            if !privilege_evidence.privileges_constrained() {
                bail!(
                    "restricted token retains {} enabled non-traverse privilege(s)",
                    privilege_evidence.restricted_enabled_non_traverse_privilege_count
                );
            }

            let stdin = open_inheritable_nul()?;
            let stdout = InheritablePipe::new()?;
            let stderr = InheritablePipe::new()?;
            let inherited_handles = [
                raw_handle(&stdin),
                raw_handle(&stdout.write),
                raw_handle(&stderr.write),
            ];
            let mut attributes = ProcThreadAttributeList::for_handles(&inherited_handles)?;

            let mut command_line = windows_command_line(&program, &request.args)?;
            let program_wide = nul_terminated(program.as_os_str(), "program")?;
            let cwd_wide = nul_terminated(cwd.as_os_str(), "cwd")?;
            let environment = windows_environment_block(request)?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
                .context("STARTUPINFOEXW size exceeds u32")?;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = raw_handle(&stdin);
            startup.StartupInfo.hStdOutput = raw_handle(&stdout.write);
            startup.StartupInfo.hStdError = raw_handle(&stderr.write);
            startup.lpAttributeList = attributes.as_ptr();
            let mut process_info = PROCESS_INFORMATION::default();
            let creation_flags = CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | EXTENDED_STARTUPINFO_PRESENT;

            // SAFETY: All pointers remain valid for the call, the mutable command line and
            // environment block are NUL-terminated, and the inherited handle list contains only
            // live inheritable child-side handles.
            let created = unsafe {
                CreateProcessAsUserW(
                    raw_handle(&restricted_token),
                    program_wide.as_ptr(),
                    command_line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    creation_flags,
                    environment.as_ptr().cast::<c_void>(),
                    cwd_wide.as_ptr(),
                    (&raw const startup.StartupInfo),
                    &raw mut process_info,
                )
            };
            if created == 0 {
                return Err(io::Error::last_os_error())
                    .context("CreateProcessAsUserW failed for restricted token");
            }

            // SAFETY: CreateProcessAsUserW returned ownership of both non-null handles.
            let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) };
            // SAFETY: CreateProcessAsUserW returned ownership of both non-null handles.
            let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
            drop(attributes);
            drop(stdin);
            drop(stdout.write);
            drop(stderr.write);
            drop(restricted_token);
            drop(process_token);

            Ok(Self {
                process: Arc::new(process),
                thread: Some(thread),
                process_id: process_info.dwProcessId,
                stdout: Some(stdout.read.into_tokio_file()),
                stderr: Some(stderr.read.into_tokio_file()),
                wait_task: None,
                status: None,
                privilege_evidence,
            })
        }

        pub(crate) fn process_id(&self) -> u32 {
            self.process_id
        }

        pub(crate) fn privilege_evidence(&self) -> RestrictedTokenPrivilegeEvidence {
            self.privilege_evidence
        }

        pub(crate) fn take_stdout(&mut self) -> Option<super::super::SupervisedExecutionPipe> {
            self.stdout
                .take()
                .map(|stdout| Box::new(stdout) as super::super::SupervisedExecutionPipe)
        }

        pub(crate) fn take_stderr(&mut self) -> Option<super::super::SupervisedExecutionPipe> {
            self.stderr
                .take()
                .map(|stderr| Box::new(stderr) as super::super::SupervisedExecutionPipe)
        }

        pub(crate) fn resume(&mut self) -> io::Result<()> {
            let Some(thread) = self.thread.take() else {
                return Err(io::Error::other(
                    "Windows restricted child was already resumed",
                ));
            };
            // SAFETY: The owned thread handle is live and belongs to the suspended child.
            let previous_suspend_count = unsafe { ResumeThread(raw_handle(&thread)) };
            if previous_suspend_count == u32::MAX {
                self.thread = Some(thread);
                return Err(io::Error::last_os_error());
            }
            drop(thread);
            self.ensure_wait_task();
            Ok(())
        }

        fn ensure_wait_task(&mut self) {
            if self.status.is_some() || self.wait_task.is_some() {
                return;
            }
            let process = Arc::clone(&self.process);
            self.wait_task = Some(tokio::task::spawn_blocking(move || {
                wait_for_process(process.as_raw_handle())
            }));
        }

        pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
            if let Some(status) = self.status {
                return Ok(status);
            }
            self.ensure_wait_task();
            let result = match self.wait_task.as_mut() {
                Some(task) => task.await,
                None => return Err(io::Error::other("Windows process wait task is unavailable")),
            };
            self.wait_task = None;
            let status = result.map_err(|error| {
                io::Error::other(format!("process wait task failed: {error}"))
            })??;
            self.status = Some(status);
            Ok(status)
        }

        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if let Some(status) = self.status {
                return Ok(Some(status));
            }
            match wait_for_process_timeout(self.process.as_raw_handle(), 0)? {
                Some(status) => {
                    self.status = Some(status);
                    Ok(Some(status))
                }
                None => Ok(None),
            }
        }

        pub(crate) fn start_kill(&mut self) -> io::Result<()> {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            // SAFETY: The process handle remains owned by self for the duration of the call.
            if unsafe {
                TerminateProcess(
                    self.process.as_raw_handle(),
                    TERMINATED_BY_SUPERVISOR_EXIT_CODE,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for NativeWindowsRestrictedChild {
        fn drop(&mut self) {
            if self.status.is_none() {
                // SAFETY: Best-effort kill-on-drop uses the still-owned process handle.
                let _ = unsafe {
                    TerminateProcess(
                        self.process.as_raw_handle(),
                        TERMINATED_BY_SUPERVISOR_EXIT_CODE,
                    )
                };
            }
        }
    }

    struct InheritablePipe {
        read: OwnedHandle,
        write: OwnedHandle,
    }

    impl InheritablePipe {
        fn new() -> Result<Self> {
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                    .context("SECURITY_ATTRIBUTES size exceeds u32")?,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let mut read: HANDLE = null_mut();
            let mut write: HANDLE = null_mut();
            // SAFETY: Output pointers and SECURITY_ATTRIBUTES are valid for the call.
            if unsafe { CreatePipe(&raw mut read, &raw mut write, &raw mut attributes, 0) } == 0 {
                return Err(io::Error::last_os_error()).context("CreatePipe failed");
            }
            // SAFETY: CreatePipe succeeded and transferred ownership of both handles.
            let read = unsafe { OwnedHandle::from_raw_handle(read) };
            // SAFETY: CreatePipe succeeded and transferred ownership of both handles.
            let write = unsafe { OwnedHandle::from_raw_handle(write) };
            // SAFETY: The live parent-side read handle is valid for SetHandleInformation.
            if unsafe { SetHandleInformation(raw_handle(&read), HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to make parent pipe handle non-inheritable");
            }
            Ok(Self { read, write })
        }
    }

    trait IntoTokioFile {
        fn into_tokio_file(self) -> TokioFile;
    }

    impl IntoTokioFile for OwnedHandle {
        fn into_tokio_file(self) -> TokioFile {
            TokioFile::from_std(std::fs::File::from(self))
        }
    }

    struct ProcThreadAttributeList {
        storage: Vec<usize>,
    }

    impl ProcThreadAttributeList {
        fn for_handles(handles: &[HANDLE]) -> Result<Self> {
            let mut bytes = 0_usize;
            // SAFETY: A null buffer is the documented sizing call; bytes is a valid out pointer.
            let _ = unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &raw mut bytes) };
            if bytes == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to size process thread attribute list");
            }
            let words = bytes.div_ceil(size_of::<usize>());
            let mut storage = vec![0_usize; words];
            // SAFETY: storage is aligned for usize and large enough for the requested byte size.
            if unsafe {
                InitializeProcThreadAttributeList(
                    storage.as_mut_ptr().cast::<c_void>(),
                    1,
                    0,
                    &raw mut bytes,
                )
            } == 0
            {
                return Err(io::Error::last_os_error())
                    .context("failed to initialize process thread attribute list");
            }
            let mut list = Self { storage };
            // SAFETY: list is initialized, and handles points to a live fixed-size handle array.
            if unsafe {
                UpdateProcThreadAttribute(
                    list.as_ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast::<c_void>(),
                    size_of_val(handles),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error())
                    .context("failed to install exact inherited handle list");
            }
            Ok(list)
        }

        fn as_ptr(&mut self) -> *mut c_void {
            self.storage.as_mut_ptr().cast::<c_void>()
        }
    }

    impl Drop for ProcThreadAttributeList {
        fn drop(&mut self) {
            // SAFETY: A successfully constructed list remains initialized until this drop.
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }

    fn canonical_file(path: &Path, label: &str) -> Result<std::path::PathBuf> {
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("failed to canonicalize Windows restricted {label}"))?;
        if !canonical.is_file() {
            bail!(
                "Windows restricted launch {label} is not a file: {}",
                canonical.display()
            );
        }
        Ok(canonical)
    }

    fn open_current_process_token() -> Result<OwnedHandle> {
        let mut token: HANDLE = null_mut();
        // SAFETY: token is a valid out pointer and GetCurrentProcess returns a pseudo-handle.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY,
                &raw mut token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("OpenProcessToken failed");
        }
        // SAFETY: OpenProcessToken succeeded and transferred ownership of a non-null handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(token) })
    }

    fn create_restricted_token(
        existing: HANDLE,
        restricting_sid: Option<&WindowsRestrictingSid>,
    ) -> Result<OwnedHandle> {
        let mut restricted: HANDLE = null_mut();
        let mut restricting_entry = restricting_sid.map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_ptr(),
            Attributes: 0,
        });
        let restricting_count = u32::from(restricting_entry.is_some());
        let restricting_entries = restricting_entry.as_mut().map_or(null(), |entry| {
            (entry as *mut SID_AND_ATTRIBUTES).cast_const()
        });
        let flags = DISABLE_MAX_PRIVILEGE
            | if restricting_sid.is_some() {
                WRITE_RESTRICTED
            } else {
                0
            };
        // SAFETY: existing is a live token, restricted is a valid out pointer, and the optional
        // restricting SID entry remains live for the duration of the call.
        if unsafe {
            CreateRestrictedToken(
                existing,
                flags,
                0,
                null(),
                0,
                null(),
                restricting_count,
                restricting_entries,
                &raw mut restricted,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("CreateRestrictedToken failed");
        }
        // SAFETY: CreateRestrictedToken succeeded and transferred ownership of the token handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(restricted) })
    }

    fn restricted_token_privilege_evidence(
        source: HANDLE,
        restricted: HANDLE,
    ) -> Result<RestrictedTokenPrivilegeEvidence> {
        let traverse_privilege = lookup_traverse_privilege()?;
        let source_enabled_non_traverse_privilege_count =
            enabled_non_traverse_privilege_count(source, traverse_privilege)?;
        let restricted_enabled_non_traverse_privilege_count =
            enabled_non_traverse_privilege_count(restricted, traverse_privilege)?;
        let restricting_sid_count = restricted_sid_count(restricted)?;
        Ok(RestrictedTokenPrivilegeEvidence {
            source_enabled_non_traverse_privilege_count,
            restricted_enabled_non_traverse_privilege_count,
            restricting_sid_count,
        })
    }

    fn restricted_sid_count(token: HANDLE) -> Result<u32> {
        let mut bytes = 0_u32;
        // SAFETY: This is the documented sizing call and bytes is a valid output pointer.
        let _ = unsafe {
            GetTokenInformation(token, TokenRestrictedSids, null_mut(), 0, &raw mut bytes)
        };
        if bytes < u32::try_from(size_of::<u32>()).expect("u32 size fits u32") {
            bail!("TokenRestrictedSids sizing returned an invalid buffer length");
        }

        let words = usize::try_from(bytes)
            .context("TokenRestrictedSids buffer length exceeds usize")?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let storage_bytes = u32::try_from(storage.len() * size_of::<usize>())
            .context("TokenRestrictedSids storage length exceeds u32")?;
        let mut returned_bytes = 0_u32;
        // SAFETY: storage is aligned and sufficiently sized for TOKEN_GROUPS, token is live, and
        // returned_bytes is a valid output pointer.
        if unsafe {
            GetTokenInformation(
                token,
                TokenRestrictedSids,
                storage.as_mut_ptr().cast::<c_void>(),
                storage_bytes,
                &raw mut returned_bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("GetTokenInformation(TokenRestrictedSids) failed");
        }
        if returned_bytes < u32::try_from(size_of::<u32>()).expect("u32 size fits u32") {
            bail!("TokenRestrictedSids returned a truncated header");
        }
        // SAFETY: GetTokenInformation initialized at least the GroupCount header field.
        Ok(unsafe { (*storage.as_ptr().cast::<TOKEN_GROUPS>()).GroupCount })
    }

    fn lookup_traverse_privilege() -> Result<windows_sys::Win32::Foundation::LUID> {
        let mut luid = windows_sys::Win32::Foundation::LUID::default();
        // SAFETY: SE_CHANGE_NOTIFY_NAME is a static NUL-terminated string and luid is a valid
        // out pointer. A null system name selects the local system.
        if unsafe { LookupPrivilegeValueW(null(), SE_CHANGE_NOTIFY_NAME, &raw mut luid) } == 0 {
            return Err(io::Error::last_os_error())
                .context("failed to resolve SeChangeNotifyPrivilege");
        }
        Ok(luid)
    }

    fn enabled_non_traverse_privilege_count(
        token: HANDLE,
        traverse_privilege: windows_sys::Win32::Foundation::LUID,
    ) -> Result<u32> {
        if token.is_null() || token == INVALID_HANDLE_VALUE {
            bail!("cannot inspect an invalid token handle");
        }

        let mut bytes = 0_u32;
        // SAFETY: This is the documented sizing call. token is live and bytes is a valid out
        // pointer; no output buffer is supplied.
        let _ =
            unsafe { GetTokenInformation(token, TokenPrivileges, null_mut(), 0, &raw mut bytes) };
        let token_privileges_header_bytes = u32::try_from(size_of::<TOKEN_PRIVILEGES>())
            .context("TOKEN_PRIVILEGES size exceeds u32")?;
        if bytes < token_privileges_header_bytes {
            bail!("TokenPrivileges sizing returned an invalid buffer length");
        }

        let words = usize::try_from(bytes)
            .context("TokenPrivileges buffer length exceeds usize")?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let storage_bytes = u32::try_from(storage.len() * size_of::<usize>())
            .context("TokenPrivileges storage length exceeds u32")?;
        let mut returned_bytes = 0_u32;
        // SAFETY: storage is suitably aligned and sized for TOKEN_PRIVILEGES, token remains live,
        // and returned_bytes is a valid out pointer.
        if unsafe {
            GetTokenInformation(
                token,
                TokenPrivileges,
                storage.as_mut_ptr().cast::<c_void>(),
                storage_bytes,
                &raw mut returned_bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("GetTokenInformation failed");
        }

        let token_privileges = storage.as_ptr().cast::<TOKEN_PRIVILEGES>();
        // SAFETY: GetTokenInformation initialized the TOKEN_PRIVILEGES header in storage.
        let count = unsafe { (*token_privileges).PrivilegeCount as usize };
        // SAFETY: token_privileges points at a complete TOKEN_PRIVILEGES header in storage.
        let privileges_ptr = unsafe {
            std::ptr::addr_of!((*token_privileges).Privileges)
                .cast::<windows_sys::Win32::Security::LUID_AND_ATTRIBUTES>()
        };
        let privilege_offset = privileges_ptr as usize - token_privileges as usize;
        let required_bytes = privilege_offset
            .checked_add(
                count
                    .checked_mul(size_of::<windows_sys::Win32::Security::LUID_AND_ATTRIBUTES>())
                    .context("TokenPrivileges entry count overflowed")?,
            )
            .context("TokenPrivileges buffer size overflowed")?;
        let returned_bytes = usize::try_from(returned_bytes)
            .context("TokenPrivileges returned length exceeds usize")?;
        let available_bytes = returned_bytes.min(storage.len() * size_of::<usize>());
        if required_bytes > available_bytes {
            bail!("TokenPrivileges returned a truncated privilege array");
        }
        // SAFETY: Privileges is the first element of the variable-length trailing array.
        let privileges = unsafe { std::slice::from_raw_parts(privileges_ptr, count) };
        let enabled_non_traverse = privileges
            .iter()
            .filter(|privilege| privilege.Attributes & SE_PRIVILEGE_ENABLED != 0)
            .filter(|privilege| {
                privilege.Luid.LowPart != traverse_privilege.LowPart
                    || privilege.Luid.HighPart != traverse_privilege.HighPart
            })
            .count();
        u32::try_from(enabled_non_traverse).context("enabled privilege count exceeds u32")
    }

    fn open_inheritable_nul() -> Result<OwnedHandle> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .context("SECURITY_ATTRIBUTES size exceeds u32")?,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let nul = ['N' as u16, 'U' as u16, 'L' as u16, 0];
        // SAFETY: nul is NUL-terminated and attributes remains valid for the call.
        let handle = unsafe {
            CreateFileW(
                nul.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &raw mut attributes,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error()).context("failed to open inheritable NUL stdin");
        }
        // SAFETY: CreateFileW succeeded and transferred ownership of the handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn wait_for_process(handle: std::os::windows::io::RawHandle) -> io::Result<ExitStatus> {
        match wait_for_process_timeout(handle, INFINITE)? {
            Some(status) => Ok(status),
            None => Err(io::Error::other(
                "infinite Windows process wait returned timeout",
            )),
        }
    }

    fn wait_for_process_timeout(
        handle: std::os::windows::io::RawHandle,
        timeout_ms: u32,
    ) -> io::Result<Option<ExitStatus>> {
        // SAFETY: handle is kept alive by an OwnedHandle for the duration of this call.
        let wait = unsafe { WaitForSingleObject(handle, timeout_ms) };
        match wait {
            WAIT_OBJECT_0 => {
                let mut exit_code = 0_u32;
                // SAFETY: handle is signaled and exit_code is a valid out pointer.
                if unsafe { GetExitCodeProcess(handle, &raw mut exit_code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(exit_code)))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "unexpected Windows process wait result {other}"
            ))),
        }
    }

    fn raw_handle(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle()
    }

    fn nul_terminated(value: &OsStr, label: &str) -> Result<Vec<u16>> {
        let mut wide = value.encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            bail!("Windows restricted launch {label} contains an embedded NUL");
        }
        wide.push(0);
        Ok(wide)
    }

    fn windows_command_line(program: &Path, args: &[String]) -> Result<Vec<u16>> {
        let mut command_line = Vec::new();
        append_quoted_windows_arg(&mut command_line, program.as_os_str())?;
        for arg in args {
            command_line.push(' ' as u16);
            append_quoted_windows_arg(&mut command_line, OsStr::new(arg))?;
        }
        command_line.push(0);
        Ok(command_line)
    }

    fn append_quoted_windows_arg(output: &mut Vec<u16>, arg: &OsStr) -> Result<()> {
        let wide = arg.encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            bail!("Windows restricted launch argument contains an embedded NUL");
        }
        let requires_quotes =
            wide.is_empty() || wide.iter().any(|unit| matches!(*unit, 0x09 | 0x20 | 0x22));
        if !requires_quotes {
            output.extend(wide);
            return Ok(());
        }

        output.push('"' as u16);
        let mut backslashes = 0_usize;
        for unit in wide {
            if unit == '\\' as u16 {
                backslashes += 1;
                continue;
            }
            if unit == '"' as u16 {
                output.extend(std::iter::repeat_n('\\' as u16, backslashes * 2 + 1));
                output.push(unit);
            } else {
                output.extend(std::iter::repeat_n('\\' as u16, backslashes));
                output.push(unit);
            }
            backslashes = 0;
        }
        output.extend(std::iter::repeat_n('\\' as u16, backslashes * 2));
        output.push('"' as u16);
        Ok(())
    }

    fn windows_environment_block(request: &ExecutionRequest) -> Result<Vec<u16>> {
        let mut entries = Vec::<(Vec<u16>, OsString, OsString)>::new();
        if request.environment_policy == ProcessEnvironmentPolicy::InheritParent {
            for (key, value) in std::env::vars_os() {
                insert_environment_entry(&mut entries, key, value, true)?;
            }
        }
        for (key, value) in &request.env {
            insert_environment_entry(
                &mut entries,
                OsString::from(key),
                OsString::from(value),
                false,
            )?;
        }

        let mut block = Vec::new();
        for (_, key, value) in entries {
            block.extend(key.encode_wide());
            block.push('=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        if block.len() == 1 {
            block.push(0);
        }
        Ok(block)
    }

    fn insert_environment_entry(
        entries: &mut Vec<(Vec<u16>, OsString, OsString)>,
        key: OsString,
        value: OsString,
        allow_parent_drive_entry: bool,
    ) -> Result<()> {
        let key_text = key.to_string_lossy();
        let has_invalid_equals = if allow_parent_drive_entry && key_text.starts_with('=') {
            key_text[1..].contains('=')
        } else {
            key_text.contains('=')
        };
        if key_text.is_empty() || has_invalid_equals {
            bail!("Windows restricted launch environment contains an invalid variable name");
        }
        let key_wide = key.encode_wide().collect::<Vec<_>>();
        if key_wide.contains(&0) || value.encode_wide().any(|unit| unit == 0) {
            bail!("Windows restricted launch environment contains an embedded NUL");
        }
        let mut insertion_index = entries.len();
        for (index, (existing_wide, _, _)) in entries.iter().enumerate() {
            match compare_environment_keys(&key_wide, existing_wide)? {
                Ordering::Equal => {
                    entries[index] = (key_wide, key, value);
                    return Ok(());
                }
                Ordering::Less => {
                    insertion_index = index;
                    break;
                }
                Ordering::Greater => {}
            }
        }
        entries.insert(insertion_index, (key_wide, key, value));
        Ok(())
    }

    fn compare_environment_keys(left: &[u16], right: &[u16]) -> Result<Ordering> {
        let left_len = i32::try_from(left.len()).context("environment key is too long")?;
        let right_len = i32::try_from(right.len()).context("environment key is too long")?;
        // SAFETY: Both slices remain live for the call, their exact lengths are supplied, and
        // CompareStringOrdinal does not retain either pointer.
        match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) }
        {
            CSTR_LESS_THAN => Ok(Ordering::Less),
            CSTR_EQUAL => Ok(Ordering::Equal),
            CSTR_GREATER_THAN => Ok(Ordering::Greater),
            _ => Err(io::Error::last_os_error())
                .context("failed to compare Windows environment variable names"),
        }
    }
}

#[cfg(windows)]
pub(super) use native::{NativeWindowsRestrictedChild, WindowsRestrictingSid};

#[cfg(test)]
#[path = "../tests/windows_restricted_tests.rs"]
mod tests;
