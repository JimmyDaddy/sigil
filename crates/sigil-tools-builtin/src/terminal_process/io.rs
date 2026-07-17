use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalOutputStream {
    Stdout,
    Stderr,
}

const CAPTURE_TERMINATION_FAILED: u8 = 1;
const CAPTURE_TERMINATION_OUTPUT_LIMIT: u8 = 2;

#[derive(Debug, Default)]
pub(super) struct TerminalCaptureLedger {
    stdout_observed_bytes: AtomicU64,
    stdout_written_bytes: AtomicU64,
    stderr_observed_bytes: AtomicU64,
    stderr_written_bytes: AtomicU64,
    termination: AtomicU8,
    limit_bytes: AtomicU64,
}

impl TerminalCaptureLedger {
    fn record_observed(&self, stream: TerminalOutputStream, bytes: u64) {
        let observed = match stream {
            TerminalOutputStream::Stdout => &self.stdout_observed_bytes,
            TerminalOutputStream::Stderr => &self.stderr_observed_bytes,
        };
        let _ = observed.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_add(bytes))
        });
    }

    fn record_written(&self, stream: TerminalOutputStream, bytes: u64) {
        let written = match stream {
            TerminalOutputStream::Stdout => &self.stdout_written_bytes,
            TerminalOutputStream::Stderr => &self.stderr_written_bytes,
        };
        let _ = written.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_add(bytes))
        });
    }

    fn record_failure(&self, failure: &TerminalCaptureFailure) {
        let termination = match failure.termination_reason() {
            TerminalOutputTerminationReason::OutputLimitExceeded => {
                CAPTURE_TERMINATION_OUTPUT_LIMIT
            }
            TerminalOutputTerminationReason::OutputCaptureFailed
            | TerminalOutputTerminationReason::OutputDrainTimeout => CAPTURE_TERMINATION_FAILED,
        };
        self.termination.fetch_max(termination, Ordering::AcqRel);
        if let Some(limit_bytes) = failure.limit_bytes() {
            let _ = self
                .limit_bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(if current == 0 {
                        limit_bytes
                    } else {
                        current.min(limit_bytes)
                    })
                });
        }
    }

    pub(super) fn omitted_observed_bytes(&self) -> u64 {
        let stdout = self
            .stdout_observed_bytes
            .load(Ordering::Acquire)
            .saturating_sub(self.stdout_written_bytes.load(Ordering::Acquire));
        let stderr = self
            .stderr_observed_bytes
            .load(Ordering::Acquire)
            .saturating_sub(self.stderr_written_bytes.load(Ordering::Acquire));
        stdout.saturating_add(stderr)
    }

    pub(super) fn total_observed_bytes(&self) -> u64 {
        self.stdout_observed_bytes
            .load(Ordering::Acquire)
            .saturating_add(self.stderr_observed_bytes.load(Ordering::Acquire))
    }

    pub(super) fn limit_bytes(&self) -> Option<u64> {
        let limit = self.limit_bytes.load(Ordering::Acquire);
        (limit > 0).then_some(limit)
    }

    pub(super) fn termination_reason(&self) -> Option<TerminalOutputTerminationReason> {
        match self.termination.load(Ordering::Acquire) {
            CAPTURE_TERMINATION_OUTPUT_LIMIT => {
                Some(TerminalOutputTerminationReason::OutputLimitExceeded)
            }
            CAPTURE_TERMINATION_FAILED => {
                Some(TerminalOutputTerminationReason::OutputCaptureFailed)
            }
            _ => None,
        }
    }
}

impl TerminalOutputStream {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalCaptureFailure {
    stream: TerminalOutputStream,
    reason: String,
    observed_bytes: u64,
    written_bytes: u64,
    limit_bytes: Option<u64>,
    termination_reason: TerminalOutputTerminationReason,
}

impl TerminalCaptureFailure {
    fn output_limit(
        stream: TerminalOutputStream,
        limit_name: &str,
        limit_bytes: u64,
        observed_bytes: u64,
        written_bytes: u64,
    ) -> Self {
        Self {
            stream,
            reason: format!(
                "terminal {limit_name} output limit exceeded: limit_bytes={limit_bytes}, observed_bytes={observed_bytes}, written_bytes={written_bytes}"
            ),
            observed_bytes,
            written_bytes,
            limit_bytes: Some(limit_bytes),
            termination_reason: TerminalOutputTerminationReason::OutputLimitExceeded,
        }
    }

    fn io(stream: TerminalOutputStream, operation: &str, error: impl std::fmt::Display) -> Self {
        Self {
            stream,
            reason: format!("terminal output {operation} failed: {error}"),
            observed_bytes: 0,
            written_bytes: 0,
            limit_bytes: None,
            termination_reason: TerminalOutputTerminationReason::OutputCaptureFailed,
        }
    }

    fn reader_panicked(stream: TerminalOutputStream, reader_kind: &str) -> Self {
        Self {
            stream,
            reason: format!("terminal {reader_kind} reader panicked"),
            observed_bytes: 0,
            written_bytes: 0,
            limit_bytes: None,
            termination_reason: TerminalOutputTerminationReason::OutputCaptureFailed,
        }
    }

    fn with_counts(mut self, observed_bytes: u64, written_bytes: u64) -> Self {
        self.observed_bytes = observed_bytes;
        self.written_bytes = written_bytes;
        self
    }

    pub(super) fn limit_bytes(&self) -> Option<u64> {
        self.limit_bytes
    }

    pub(super) fn termination_reason(&self) -> TerminalOutputTerminationReason {
        self.termination_reason
    }
}

impl std::fmt::Display for TerminalCaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.stream.as_str(), self.reason)
    }
}

impl std::error::Error for TerminalCaptureFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CaptureOutcome {
    pub(super) observed_bytes: u64,
    pub(super) written_bytes: u64,
}

pub(super) struct CombinedOutputWriter {
    file: File,
    written_bytes: u64,
    limit_bytes: u64,
}

impl CombinedOutputWriter {
    pub(super) fn new(file: File, limit_bytes: u64) -> Self {
        Self {
            file,
            written_bytes: 0,
            limit_bytes,
        }
    }
}

const CURSOR_POSITION_QUERY: &[u8] = b"\x1b[6n";
const PRIVATE_CURSOR_POSITION_QUERY: &[u8] = b"\x1b[?6n";
const CURSOR_POSITION_RESPONSE: &[u8] = b"\x1b[1;1R";
const TERMINAL_QUERY_TAIL_BYTES: usize = PRIVATE_CURSOR_POSITION_QUERY.len() - 1;

#[derive(Default)]
pub(super) struct TerminalQueryResponder {
    io_control: Option<std::sync::Weak<PtyIoControl>>,
    tail: Vec<u8>,
}

impl TerminalQueryResponder {
    pub(super) fn new(io_control: Option<std::sync::Weak<PtyIoControl>>) -> Self {
        Self {
            io_control,
            tail: Vec::new(),
        }
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(io_control) = self.io_control.as_ref().and_then(std::sync::Weak::upgrade) else {
            return Ok(());
        };
        let input_tx = io_control.input_tx.lock().map_err(|_| {
            anyhow!("terminal PTY input lock poisoned while responding to cursor query")
        })?;
        let Some(input_tx) = input_tx.as_ref() else {
            return Ok(());
        };
        let previous_tail_len = self.tail.len();
        self.tail.extend_from_slice(bytes);
        let response_count = [CURSOR_POSITION_QUERY, PRIVATE_CURSOR_POSITION_QUERY]
            .into_iter()
            .map(|query| count_new_terminal_queries(&self.tail, previous_tail_len, query))
            .sum::<usize>();
        for _ in 0..response_count {
            input_tx
                .try_send(CURSOR_POSITION_RESPONSE.to_vec())
                .map_err(|error| match error {
                    std_mpsc::TrySendError::Full(_) => {
                        anyhow!("terminal PTY input queue is full while responding to cursor query")
                    }
                    std_mpsc::TrySendError::Disconnected(_) => anyhow!(
                        "terminal PTY input channel closed while responding to cursor query"
                    ),
                })?;
        }
        if self.tail.len() > TERMINAL_QUERY_TAIL_BYTES {
            self.tail
                .drain(..self.tail.len().saturating_sub(TERMINAL_QUERY_TAIL_BYTES));
        }
        Ok(())
    }
}

fn count_new_terminal_queries(bytes: &[u8], previous_len: usize, query: &[u8]) -> usize {
    bytes
        .windows(query.len())
        .enumerate()
        .filter(|(index, window)| *window == query && index + query.len() > previous_len)
        .count()
}

pub(super) fn spawn_pty_read_thread(
    reader: Box<dyn Read + Send>,
    stream_path: PathBuf,
    output_path: PathBuf,
    limits: TerminalArtifactLimits,
    terminal_io_control: Option<std::sync::Weak<PtyIoControl>>,
    capture_ledger: Arc<TerminalCaptureLedger>,
    capture_failure_tx: mpsc::UnboundedSender<TerminalCaptureFailure>,
) -> ThreadJoinHandle<Result<CaptureOutcome>> {
    let panic_capture_ledger = Arc::clone(&capture_ledger);
    let panic_failure_tx = capture_failure_tx.clone();
    std::thread::spawn(move || {
        match catch_unwind(AssertUnwindSafe(|| {
            capture_pty_reader(
                reader,
                stream_path,
                output_path,
                limits,
                terminal_io_control,
                capture_ledger,
                capture_failure_tx,
            )
        })) {
            Ok(result) => result,
            Err(_) => report_capture_failure(
                &panic_failure_tx,
                &panic_capture_ledger,
                TerminalCaptureFailure::reader_panicked(TerminalOutputStream::Stdout, "pty output"),
            ),
        }
    })
}

pub(super) fn spawn_pty_input_thread(
    mut writer: Box<dyn Write + Send>,
    input_rx: std_mpsc::Receiver<Vec<u8>>,
) {
    let _ = std::thread::spawn(move || -> Result<u64> {
        let mut total = 0u64;
        while let Ok(input) = input_rx.recv() {
            writer
                .write_all(&input)
                .context("failed to write terminal pty input")?;
            writer
                .flush()
                .context("failed to flush terminal pty input")?;
            total += input.len() as u64;
        }
        Ok(total)
    });
}

pub(super) fn join_pty_read_thread(
    read_thread: ThreadJoinHandle<Result<CaptureOutcome>>,
) -> Option<String> {
    match read_thread.join() {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("terminal pty read thread panicked".to_owned()),
    }
}

pub(super) fn capture_pty_reader(
    mut reader: Box<dyn Read + Send>,
    stream_path: PathBuf,
    output_path: PathBuf,
    limits: TerminalArtifactLimits,
    terminal_io_control: Option<std::sync::Weak<PtyIoControl>>,
    capture_ledger: Arc<TerminalCaptureLedger>,
    capture_failure_tx: mpsc::UnboundedSender<TerminalCaptureFailure>,
) -> Result<CaptureOutcome> {
    let stream = TerminalOutputStream::Stdout;
    let mut terminal_query_responder = TerminalQueryResponder::new(terminal_io_control);
    let mut stream_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
    {
        Ok(file) => file,
        Err(error) => {
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::io(stream, "open stream artifact", error),
            );
        }
    };
    let mut output_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
    {
        Ok(file) => file,
        Err(error) => {
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::io(stream, "open combined artifact", error),
            );
        }
    };
    let mut observed_bytes = 0u64;
    let mut written_bytes = 0u64;
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if is_pty_eof_error(&error) => break,
            Err(error) => {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "read pty stream", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
        };
        if let Err(error) = terminal_query_responder.observe(&buffer[..read]) {
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::io(stream, "respond to terminal query", error)
                    .with_counts(observed_bytes, written_bytes),
            );
        }
        observed_bytes = observed_bytes.saturating_add(read as u64);
        capture_ledger.record_observed(stream, read as u64);
        let remaining_stream = limits.stream_bytes.saturating_sub(written_bytes);
        let remaining_combined = limits.combined_bytes.saturating_sub(written_bytes);
        let allowed = read.min(remaining_stream.min(remaining_combined) as usize);
        if allowed > 0 {
            if let Err(error) = stream_file.write_all(&buffer[..allowed]) {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "write stream artifact", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
            if let Err(error) = output_file.write_all(&buffer[..allowed]) {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "write combined artifact", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
            written_bytes = written_bytes.saturating_add(allowed as u64);
            capture_ledger.record_written(stream, allowed as u64);
        }
        if allowed < read {
            let (limit_name, limit_bytes) = if remaining_stream <= remaining_combined {
                ("stdout artifact", limits.stream_bytes)
            } else {
                ("combined artifact", limits.combined_bytes)
            };
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::output_limit(
                    stream,
                    limit_name,
                    limit_bytes,
                    observed_bytes,
                    written_bytes,
                ),
            );
        }
    }
    if let Err(error) = stream_file.flush() {
        return report_capture_failure(
            &capture_failure_tx,
            &capture_ledger,
            TerminalCaptureFailure::io(stream, "flush stream artifact", error)
                .with_counts(observed_bytes, written_bytes),
        );
    }
    if let Err(error) = output_file.flush() {
        return report_capture_failure(
            &capture_failure_tx,
            &capture_ledger,
            TerminalCaptureFailure::io(stream, "flush combined artifact", error)
                .with_counts(observed_bytes, written_bytes),
        );
    }
    Ok(CaptureOutcome {
        observed_bytes,
        written_bytes,
    })
}

pub(super) fn is_pty_eof_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
    ) || error.raw_os_error() == Some(5)
}

pub(super) fn spawn_capture_task<R>(
    reader: Option<R>,
    stream: TerminalOutputStream,
    stream_path: PathBuf,
    output_file: Arc<Mutex<CombinedOutputWriter>>,
    limits: TerminalArtifactLimits,
    capture_ledger: Arc<TerminalCaptureLedger>,
    capture_failure_tx: mpsc::UnboundedSender<TerminalCaptureFailure>,
) -> JoinHandle<Result<CaptureOutcome>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(capture_stream(
        reader,
        stream,
        stream_path,
        output_file,
        limits,
        capture_ledger,
        capture_failure_tx,
    ))
}

pub(super) async fn capture_stream<R>(
    mut reader: Option<R>,
    stream: TerminalOutputStream,
    stream_path: PathBuf,
    output_file: Arc<Mutex<CombinedOutputWriter>>,
    limits: TerminalArtifactLimits,
    capture_ledger: Arc<TerminalCaptureLedger>,
    capture_failure_tx: mpsc::UnboundedSender<TerminalCaptureFailure>,
) -> Result<CaptureOutcome>
where
    R: AsyncRead + Unpin,
{
    let mut stream_file = match open_append_file(&stream_path).await {
        Ok(file) => file,
        Err(error) => {
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::io(stream, "open stream artifact", error),
            );
        }
    };
    let Some(reader) = reader.as_mut() else {
        return Ok(CaptureOutcome {
            observed_bytes: 0,
            written_bytes: 0,
        });
    };
    let mut observed_bytes = 0u64;
    let mut written_bytes = 0u64;
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "read stream", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
        };
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read as u64);
        capture_ledger.record_observed(stream, read as u64);
        let mut combined = output_file.lock().await;
        let remaining_stream = limits.stream_bytes.saturating_sub(written_bytes);
        let remaining_combined = combined.limit_bytes.saturating_sub(combined.written_bytes);
        let allowed = read.min(remaining_stream.min(remaining_combined) as usize);
        if allowed > 0 {
            if let Err(error) = stream_file.write_all(&buffer[..allowed]).await {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "write stream artifact", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
            if let Err(error) = combined.file.write_all(&buffer[..allowed]).await {
                return report_capture_failure(
                    &capture_failure_tx,
                    &capture_ledger,
                    TerminalCaptureFailure::io(stream, "write combined artifact", error)
                        .with_counts(observed_bytes, written_bytes),
                );
            }
            written_bytes = written_bytes.saturating_add(allowed as u64);
            combined.written_bytes = combined.written_bytes.saturating_add(allowed as u64);
            capture_ledger.record_written(stream, allowed as u64);
        }
        if allowed < read {
            let (limit_name, limit_bytes) = if remaining_stream <= remaining_combined {
                ("stream artifact", limits.stream_bytes)
            } else {
                ("combined artifact", combined.limit_bytes)
            };
            drop(combined);
            return report_capture_failure(
                &capture_failure_tx,
                &capture_ledger,
                TerminalCaptureFailure::output_limit(
                    stream,
                    limit_name,
                    limit_bytes,
                    observed_bytes,
                    written_bytes,
                ),
            );
        }
    }
    if let Err(error) = stream_file.flush().await {
        return report_capture_failure(
            &capture_failure_tx,
            &capture_ledger,
            TerminalCaptureFailure::io(stream, "flush stream artifact", error)
                .with_counts(observed_bytes, written_bytes),
        );
    }
    let mut combined = output_file.lock().await;
    if let Err(error) = combined.file.flush().await {
        return report_capture_failure(
            &capture_failure_tx,
            &capture_ledger,
            TerminalCaptureFailure::io(stream, "flush combined artifact", error)
                .with_counts(observed_bytes, written_bytes),
        );
    }
    Ok(CaptureOutcome {
        observed_bytes,
        written_bytes,
    })
}

fn report_capture_failure<T>(
    capture_failure_tx: &mpsc::UnboundedSender<TerminalCaptureFailure>,
    capture_ledger: &TerminalCaptureLedger,
    failure: TerminalCaptureFailure,
) -> Result<T> {
    capture_ledger.record_failure(&failure);
    let _ = capture_failure_tx.send(failure.clone());
    Err(failure.into())
}

pub(super) async fn create_empty_log_files(artifacts: &TerminalTaskArtifacts) -> Result<()> {
    for path in [
        &artifacts.absolute_output,
        &artifacts.absolute_stdout,
        &artifacts.absolute_stderr,
    ] {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

pub(super) async fn open_append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))
}

pub(super) async fn write_task_meta(path: &Path, entry: &TerminalTaskEntry) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(entry).context("failed to serialize terminal task meta")?;
    fs::write(path, bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(unix)]
pub(super) fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn configure_process_group(_command: &mut Command) {}
