use anyhow::Result;
use sigil_kernel::{
    ExecutionOutputReceipt, ExecutionRequest, ExecutionResourceReceipt, ProcessEnvironmentPolicy,
};

#[cfg(windows)]
#[path = "windows_restricted_filesystem.rs"]
mod filesystem;

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
            ERROR_SUCCESS, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
            LocalFree, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
        Security::{
            ACL,
            Authorization::{
                ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW,
                TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
            },
            CopySid, CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, FreeSid, GetLengthSid,
            GetTokenInformation,
            Isolation::DeriveAppContainerSidFromAppContainerName,
            LookupPrivilegeValueW, PSID, SE_CHANGE_NOTIFY_NAME, SE_PRIVILEGE_ENABLED,
            SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, SetTokenInformation,
            TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
            TOKEN_GROUPS, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenDefaultDacl, TokenGroups,
            TokenIsAppContainer, TokenPrivileges, TokenRestrictedSids, WRITE_RESTRICTED,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
                CreateProcessAsUserW, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
                InitializeProcThreadAttributeList, OpenProcessToken,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    };

    const TERMINATED_BY_SUPERVISOR_EXIT_CODE: u32 = 1;
    const SE_GROUP_LOGON_ID_MASK: u32 = 0xC000_0000;
    const PRIVATE_APP_CONTAINER_NAME: &str = "Sigil.Rfc0041.PrivateProbe.V1";
    #[derive(Clone, Copy)]
    pub(crate) struct RestrictedTokenPrivilegeEvidence {
        pub(crate) source_enabled_non_traverse_privilege_count: u32,
        pub(crate) restricted_enabled_non_traverse_privilege_count: u32,
        pub(crate) restricting_sid_count: u32,
    }

    pub(crate) struct WindowsRestrictingSid {
        sid: PSID,
        value: String,
    }

    struct CopiedSid {
        storage: Vec<usize>,
    }

    impl CopiedSid {
        fn as_ptr(&self) -> PSID {
            self.storage.as_ptr().cast_mut().cast::<c_void>()
        }
    }

    pub(crate) struct WindowsAppContainerSid {
        sid: PSID,
    }

    impl WindowsAppContainerSid {
        pub(crate) fn derive_private_probe() -> Result<Self> {
            let name = nul_terminated(OsStr::new(PRIVATE_APP_CONTAINER_NAME), "AppContainer name")?;
            let mut sid: PSID = null_mut();
            // SAFETY: name is NUL-terminated and sid is a valid output pointer. The returned SID
            // is released with FreeSid in Drop.
            let status =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
            if status < 0 {
                bail!("failed to derive private AppContainer SID: HRESULT {status:#010x}");
            }
            if sid.is_null() {
                bail!("AppContainer SID derivation returned a null SID");
            }
            Ok(Self { sid })
        }

        fn as_ptr(&self) -> PSID {
            self.sid
        }
    }

    impl Drop for WindowsAppContainerSid {
        fn drop(&mut self) {
            if !self.sid.is_null() {
                // SAFETY: DeriveAppContainerSidFromAppContainerName allocated this SID with
                // AllocateAndInitializeSid-compatible ownership.
                let _ = unsafe { FreeSid(self.sid) };
            }
        }
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

        #[cfg(test)]
        pub(crate) fn new_everyone() -> Result<Self> {
            Self::from_string("S-1-1-0")
        }

        pub(super) fn from_string(value: &str) -> Result<Self> {
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
            Ok(Self {
                sid,
                value: value.to_owned(),
            })
        }

        pub(crate) fn as_ptr(&self) -> PSID {
            self.sid
        }

        pub(crate) fn as_str(&self) -> &str {
            &self.value
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
        app_container: bool,
    }

    impl NativeWindowsRestrictedChild {
        pub(super) fn spawn(request: &ExecutionRequest) -> Result<Self> {
            Self::spawn_with_security(request, None, None)
        }

        pub(crate) fn spawn_with_restricting_sid(
            request: &ExecutionRequest,
            restricting_sid: &WindowsRestrictingSid,
        ) -> Result<Self> {
            Self::spawn_with_security(request, Some(restricting_sid), None)
        }

        pub(crate) fn spawn_with_app_container(
            request: &ExecutionRequest,
            app_container_sid: &WindowsAppContainerSid,
        ) -> Result<Self> {
            Self::spawn_with_security(request, None, Some(app_container_sid))
        }

        fn spawn_with_security(
            request: &ExecutionRequest,
            restricting_sid: Option<&WindowsRestrictingSid>,
            app_container_sid: Option<&WindowsAppContainerSid>,
        ) -> Result<Self> {
            if restricting_sid.is_some() && app_container_sid.is_some() {
                bail!("Windows restricted token and AppContainer launch modes are exclusive");
            }
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
            let restricted_token = app_container_sid
                .is_none()
                .then(|| create_restricted_token(raw_handle(&process_token), restricting_sid))
                .transpose()?;
            let restricted_privilege_evidence = restricted_token
                .as_ref()
                .map(|token| {
                    restricted_token_privilege_evidence(
                        raw_handle(&process_token),
                        raw_handle(token),
                    )
                })
                .transpose()?;
            if let Some(evidence) = restricted_privilege_evidence
                && !evidence.privileges_constrained()
            {
                bail!(
                    "restricted token retains {} enabled non-traverse privilege(s)",
                    evidence.restricted_enabled_non_traverse_privilege_count
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
            let mut security_capabilities = app_container_sid.map(|sid| SECURITY_CAPABILITIES {
                AppContainerSid: sid.as_ptr(),
                Capabilities: null_mut(),
                CapabilityCount: 0,
                Reserved: 0,
            });
            let mut attributes = ProcThreadAttributeList::for_launch(
                &inherited_handles,
                security_capabilities.as_mut(),
            )?;

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

            // SAFETY: All pointers and optional security capabilities remain live for the call,
            // the command line and environment are NUL-terminated, and the inherited handle list
            // contains only live inheritable child-side handles.
            let created = match restricted_token.as_ref() {
                Some(token) => unsafe {
                    CreateProcessAsUserW(
                        raw_handle(token),
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
                },
                None => unsafe {
                    CreateProcessW(
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
                },
            };
            if created == 0 {
                let label = if app_container_sid.is_some() {
                    "CreateProcessW failed for AppContainer"
                } else {
                    "CreateProcessAsUserW failed for restricted token"
                };
                return Err(io::Error::last_os_error()).context(label);
            }

            // SAFETY: process creation returned ownership of both non-null handles.
            let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) };
            // SAFETY: process creation returned ownership of both non-null handles.
            let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
            let (privilege_evidence, app_container) = match app_container_sid {
                Some(_) => {
                    let inspection = (|| -> Result<RestrictedTokenPrivilegeEvidence> {
                        let child_token =
                            open_process_token(raw_handle(&process), "AppContainer child")?;
                        if !token_is_app_container(raw_handle(&child_token))? {
                            bail!("Windows AppContainer launch produced a non-AppContainer token");
                        }
                        let evidence = restricted_token_privilege_evidence(
                            raw_handle(&process_token),
                            raw_handle(&child_token),
                        )?;
                        if !evidence.privileges_constrained() {
                            bail!(
                                "Windows AppContainer child retains {} enabled non-traverse privilege(s)",
                                evidence.restricted_enabled_non_traverse_privilege_count
                            );
                        }
                        Ok(evidence)
                    })();
                    let evidence = match inspection {
                        Ok(evidence) => evidence,
                        Err(error) => {
                            // SAFETY: the process remains suspended and process is live.
                            let _ = unsafe {
                                TerminateProcess(
                                    raw_handle(&process),
                                    TERMINATED_BY_SUPERVISOR_EXIT_CODE,
                                )
                            };
                            return Err(error);
                        }
                    };
                    (evidence, true)
                }
                None => match restricted_privilege_evidence {
                    Some(evidence) => (evidence, false),
                    None => {
                        // SAFETY: the process remains suspended and process is live.
                        let _ = unsafe {
                            TerminateProcess(
                                raw_handle(&process),
                                TERMINATED_BY_SUPERVISOR_EXIT_CODE,
                            )
                        };
                        bail!("restricted token privilege evidence is unavailable");
                    }
                },
            };
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
                app_container,
            })
        }

        pub(crate) fn process_id(&self) -> u32 {
            self.process_id
        }

        pub(crate) fn privilege_evidence(&self) -> RestrictedTokenPrivilegeEvidence {
            self.privilege_evidence
        }

        pub(crate) fn is_app_container(&self) -> bool {
            self.app_container
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
        fn for_launch(
            handles: &[HANDLE],
            security_capabilities: Option<&mut SECURITY_CAPABILITIES>,
        ) -> Result<Self> {
            let attribute_count = if security_capabilities.is_some() {
                2
            } else {
                1
            };
            let mut bytes = 0_usize;
            // SAFETY: A null buffer is the documented sizing call; bytes is a valid out pointer.
            let _ = unsafe {
                InitializeProcThreadAttributeList(null_mut(), attribute_count, 0, &raw mut bytes)
            };
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
                    attribute_count,
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
            if let Some(capabilities) = security_capabilities {
                // SAFETY: list is initialized and capabilities, including its SID, remains live
                // until after CreateProcessW returns.
                if unsafe {
                    UpdateProcThreadAttribute(
                        list.as_ptr(),
                        0,
                        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                        (capabilities as *mut SECURITY_CAPABILITIES).cast::<c_void>(),
                        size_of::<SECURITY_CAPABILITIES>(),
                        null_mut(),
                        null(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error())
                        .context("failed to install AppContainer security capabilities");
                }
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
        // SAFETY: GetCurrentProcess returns a non-owning pseudo-handle valid in this process.
        let process = unsafe { GetCurrentProcess() };
        open_process_token_with_access(
            process,
            TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY,
            "current process",
        )
    }

    fn open_process_token(process: HANDLE, label: &str) -> Result<OwnedHandle> {
        open_process_token_with_access(process, TOKEN_QUERY, label)
    }

    fn open_process_token_with_access(
        process: HANDLE,
        access: u32,
        label: &str,
    ) -> Result<OwnedHandle> {
        let mut token: HANDLE = null_mut();
        // SAFETY: process is a live process or pseudo-handle and token is a valid output pointer.
        if unsafe { OpenProcessToken(process, access, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("OpenProcessToken failed for {label}"));
        }
        // SAFETY: OpenProcessToken succeeded and transferred ownership of a non-null handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(token) })
    }

    fn create_restricted_token(
        existing: HANDLE,
        restricting_sid: Option<&WindowsRestrictingSid>,
    ) -> Result<OwnedHandle> {
        let mut restricted: HANDLE = null_mut();
        let logon_sid = restricting_sid
            .map(|_| current_logon_sid(existing))
            .transpose()?;
        let everyone_sid = restricting_sid
            .map(|_| WindowsRestrictingSid::from_string("S-1-1-0"))
            .transpose()?;
        let restricting_entries = match (restricting_sid, logon_sid.as_ref(), everyone_sid.as_ref())
        {
            (Some(capability), Some(logon), Some(everyone)) => vec![
                SID_AND_ATTRIBUTES {
                    Sid: capability.as_ptr(),
                    Attributes: 0,
                },
                SID_AND_ATTRIBUTES {
                    Sid: logon.as_ptr(),
                    Attributes: 0,
                },
                SID_AND_ATTRIBUTES {
                    Sid: everyone.as_ptr(),
                    Attributes: 0,
                },
            ],
            (None, None, None) => Vec::new(),
            _ => bail!("incomplete Windows restricting SID set"),
        };
        let restricting_count = u32::try_from(restricting_entries.len())
            .context("restricting SID count exceeds u32")?;
        let restricting_entries_ptr = if restricting_entries.is_empty() {
            null()
        } else {
            restricting_entries.as_ptr()
        };
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
                restricting_entries_ptr,
                &raw mut restricted,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("CreateRestrictedToken failed");
        }
        // SAFETY: CreateRestrictedToken succeeded and transferred ownership of the token handle.
        let restricted = unsafe { OwnedHandle::from_raw_handle(restricted) };
        if let (Some(capability), Some(logon), Some(everyone)) =
            (restricting_sid, logon_sid.as_ref(), everyone_sid.as_ref())
        {
            set_restricted_token_default_dacl(
                raw_handle(&restricted),
                &[capability.as_ptr(), logon.as_ptr(), everyone.as_ptr()],
            )?;
        }
        Ok(restricted)
    }

    fn set_restricted_token_default_dacl(token: HANDLE, sids: &[PSID]) -> Result<()> {
        if sids.is_empty() {
            return Ok(());
        }

        // This DACL applies only to securable objects created by the restricted process (for
        // example runtime synchronization objects and anonymous-pipe internals). It is not a
        // filesystem-root grant: R41.2 root ACEs are managed separately and must remain minimal.
        let entries = sids
            .iter()
            .map(|sid| EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_ALL,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: 0,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: 0,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_UNKNOWN,
                    ptstrName: (*sid).cast::<u16>(),
                },
            })
            .collect::<Vec<_>>();
        let entry_count = u32::try_from(entries.len()).context("default DACL count exceeds u32")?;
        let mut acl: *mut ACL = null_mut();
        // SAFETY: entries and their referenced SIDs remain live for the call; acl is a valid
        // output pointer and is released with LocalFree on every path after allocation.
        let status =
            unsafe { SetEntriesInAclW(entry_count, entries.as_ptr(), null(), &raw mut acl) };
        if status != ERROR_SUCCESS {
            bail!("SetEntriesInAclW failed for restricted token default DACL: {status}");
        }
        if acl.is_null() {
            bail!("SetEntriesInAclW returned a null restricted token default DACL");
        }

        let info = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
        // SAFETY: token is live and has TOKEN_ADJUST_DEFAULT access. info and acl remain valid for
        // the duration of the call; SetTokenInformation copies the DACL into the token.
        let updated = unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                (&raw const info).cast::<c_void>(),
                u32::try_from(size_of::<TOKEN_DEFAULT_DACL>())
                    .expect("TOKEN_DEFAULT_DACL size fits u32"),
            )
        };
        let update_error = (updated == 0).then(io::Error::last_os_error);
        // SAFETY: SetEntriesInAclW allocated acl with LocalAlloc.
        let _ = unsafe { LocalFree(acl.cast::<c_void>()) };
        if let Some(error) = update_error {
            return Err(error).context("SetTokenInformation(TokenDefaultDacl) failed");
        }
        Ok(())
    }

    fn current_logon_sid(token: HANDLE) -> Result<CopiedSid> {
        let mut bytes = 0_u32;
        // SAFETY: This is the documented sizing call and bytes is a valid output pointer.
        let _ = unsafe { GetTokenInformation(token, TokenGroups, null_mut(), 0, &raw mut bytes) };
        if bytes < u32::try_from(size_of::<TOKEN_GROUPS>()).expect("TOKEN_GROUPS size fits u32") {
            bail!("TokenGroups sizing returned an invalid buffer length");
        }
        let words = usize::try_from(bytes)
            .context("TokenGroups buffer length exceeds usize")?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let storage_bytes = u32::try_from(storage.len() * size_of::<usize>())
            .context("TokenGroups storage length exceeds u32")?;
        let mut returned_bytes = 0_u32;
        // SAFETY: storage is aligned and sized for TOKEN_GROUPS, token remains live, and
        // returned_bytes is a valid output pointer.
        if unsafe {
            GetTokenInformation(
                token,
                TokenGroups,
                storage.as_mut_ptr().cast::<c_void>(),
                storage_bytes,
                &raw mut returned_bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("GetTokenInformation(TokenGroups) failed");
        }
        let token_groups = storage.as_ptr().cast::<TOKEN_GROUPS>();
        // SAFETY: GetTokenInformation initialized a TOKEN_GROUPS header in storage.
        let count = unsafe { (*token_groups).GroupCount as usize };
        // SAFETY: token_groups points at a complete TOKEN_GROUPS header in storage.
        let groups_ptr =
            unsafe { std::ptr::addr_of!((*token_groups).Groups).cast::<SID_AND_ATTRIBUTES>() };
        let groups_offset = groups_ptr as usize - token_groups as usize;
        let required_bytes = groups_offset
            .checked_add(
                count
                    .checked_mul(size_of::<SID_AND_ATTRIBUTES>())
                    .context("TokenGroups entry count overflowed")?,
            )
            .context("TokenGroups buffer size overflowed")?;
        let returned_bytes =
            usize::try_from(returned_bytes).context("TokenGroups returned length exceeds usize")?;
        if required_bytes > returned_bytes.min(storage.len() * size_of::<usize>()) {
            bail!("TokenGroups returned a truncated group array");
        }
        // SAFETY: The validated variable-length array contains count SID_AND_ATTRIBUTES entries.
        let groups = unsafe { std::slice::from_raw_parts(groups_ptr, count) };
        let logon_sid = groups
            .iter()
            .find(|group| group.Attributes & SE_GROUP_LOGON_ID_MASK == SE_GROUP_LOGON_ID_MASK)
            .map(|group| group.Sid)
            .context("current token does not contain a logon SID")?;
        copy_sid(logon_sid)
    }

    fn copy_sid(source: PSID) -> Result<CopiedSid> {
        // SAFETY: source points into the validated TokenGroups buffer for this call.
        let bytes = unsafe { GetLengthSid(source) };
        if bytes == 0 {
            return Err(io::Error::last_os_error()).context("GetLengthSid failed for logon SID");
        }
        let words = usize::try_from(bytes)
            .context("logon SID length exceeds usize")?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        // SAFETY: storage is aligned and large enough for bytes, and source is a valid SID.
        if unsafe { CopySid(bytes, storage.as_mut_ptr().cast::<c_void>(), source) } == 0 {
            return Err(io::Error::last_os_error()).context("CopySid failed for logon SID");
        }
        Ok(CopiedSid { storage })
    }

    fn token_is_app_container(token: HANDLE) -> Result<bool> {
        let mut value = 0_u32;
        let mut returned = 0_u32;
        // SAFETY: token is live and value/returned are valid output buffers of the documented size.
        if unsafe {
            GetTokenInformation(
                token,
                TokenIsAppContainer,
                (&raw mut value).cast::<c_void>(),
                u32::try_from(size_of::<u32>()).expect("u32 size fits u32"),
                &raw mut returned,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("GetTokenInformation(TokenIsAppContainer) failed");
        }
        if returned != u32::try_from(size_of::<u32>()).expect("u32 size fits u32") {
            bail!("TokenIsAppContainer returned an unexpected buffer length");
        }
        Ok(value != 0)
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
pub(super) use filesystem::WindowsFilesystemGrant;
#[cfg(windows)]
pub(super) use native::{
    NativeWindowsRestrictedChild, RestrictedTokenPrivilegeEvidence, WindowsAppContainerSid,
    WindowsRestrictingSid,
};

#[cfg(test)]
#[path = "../tests/windows_restricted_tests.rs"]
mod tests;
