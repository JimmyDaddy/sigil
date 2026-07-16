use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle as ThreadJoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupReceipt,
    ExecutionCleanupStatus, ExecutionConfig, ExecutionRequest, ExecutionSandboxFallback,
    ExecutionSandboxProfile, TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind,
    TerminalOutputTerminationReason, TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId,
    TerminalTaskStatus, terminal_cleanup_receipt_for_status,
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    process::{Child as TokioChild, Command},
    sync::{Mutex, mpsc, oneshot},
    task::{self, JoinHandle},
    time::{Duration, sleep, timeout},
};

use crate::constants::HARD_TERMINAL_READ_LIMIT_BYTES;
use crate::execution_backends::{
    LinuxBubblewrapExecutionBackend, MacosSeatbeltExecutionBackend,
    ensure_linux_bubblewrap_available, ensure_macos_seatbelt_available, find_executable_on_path,
    linux_bubblewrap_args, macos_seatbelt_workspace_write_profile,
};
use crate::path::{
    absolute_path_from, canonical_workspace_root, lexically_normalize_path, resolve_existing_prefix,
};

mod config; // public DTOs and terminal execution policy.
mod cwd; // workspace-confined terminal cwd resolution.
mod io; // process/PTY log capture and artifact file I/O.
mod manager; // task registry, lifecycle entrypoints, and permissions.
mod output; // bounded output reads, previews, and hashes.
mod worker; // process/PTY worker loops, cancellation, and finalization.

use config::{
    DEFAULT_CANCEL_GRACE_MS, DEFAULT_TERMINAL_PREVIEW_LIMIT_BYTES, PTY_CANCEL_POLL_INTERVAL_MS,
    TERMINAL_PTY_INPUT_QUEUE_BOUND, TERMINAL_TASK_META_FILE, TERMINAL_TASK_OUTPUT_FILE,
    TERMINAL_TASK_STDERR_FILE, TERMINAL_TASK_STDOUT_FILE, TerminalArtifactLimits,
    TerminalPtyCommandSpec, TerminalPtyExecution,
};
use cwd::{ResolvedTerminalCwd, resolve_terminal_cwd};
use io::{
    CombinedOutputWriter, TerminalCaptureFailure, TerminalCaptureLedger, TerminalOutputStream,
    configure_process_group, create_empty_log_files, join_pty_read_thread, open_append_file,
    spawn_capture_task, spawn_pty_input_thread, spawn_pty_read_thread, write_task_meta,
};
use manager::{CancelCommand, TerminalTaskStartPlan};
use output::{LogSummary, current_epoch_ms, read_terminal_output_log, summarize_log};
use worker::{
    PtyWorker, TerminalWorker, cancel_pty_task, run_pty_worker, run_terminal_worker,
    spawn_pty_runtime,
};

const TERMINAL_CLEANUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
const TERMINAL_CLEANUP_WAIT_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
use io::{capture_pty_reader, capture_stream, is_pty_eof_error};
#[cfg(test)]
use manager::{ManagedTerminalTask, TerminalTaskControl};
#[cfg(test)]
use worker::{
    PtyWaitOutcome, cancel_child, finalize_terminal_task, send_terminate_signal,
    status_from_pty_wait_result, status_from_wait_result, wait_for_terminal_summary,
};

pub use config::{
    MAX_TERMINAL_INPUT_BYTES, TERMINAL_TASK_ARTIFACT_ROOT, TerminalBackendKind,
    TerminalExecutionConfig, TerminalInputResult, TerminalPtySize, TerminalReadResult,
    TerminalResizeResult, TerminalStartRequest, TerminalTaskArtifacts,
    TerminalTaskPermissionContext,
};
pub use manager::TerminalProcessManager;

#[cfg(test)]
#[path = "tests/terminal_process_tests.rs"]
mod tests;
