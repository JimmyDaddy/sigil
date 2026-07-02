use super::*;

pub(super) fn spawn_pty_read_thread(
    reader: Box<dyn Read + Send>,
    stream_path: PathBuf,
    output_path: PathBuf,
) -> ThreadJoinHandle<Result<u64>> {
    std::thread::spawn(move || capture_pty_reader(reader, stream_path, output_path))
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

pub(super) fn join_pty_read_thread(read_thread: ThreadJoinHandle<Result<u64>>) -> Option<String> {
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
) -> Result<u64> {
    let mut stream_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
        .with_context(|| format!("failed to open {}", stream_path.display()))?;
    let mut output_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
        .with_context(|| format!("failed to open {}", output_path.display()))?;
    let mut total = 0u64;
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if is_pty_eof_error(&error) => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to read terminal pty stream for {}",
                        stream_path.display()
                    )
                });
            }
        };
        stream_file
            .write_all(&buffer[..read])
            .with_context(|| format!("failed to write {}", stream_path.display()))?;
        output_file
            .write_all(&buffer[..read])
            .context("failed to write terminal pty combined output log")?;
        total += read as u64;
    }
    stream_file
        .flush()
        .with_context(|| format!("failed to flush {}", stream_path.display()))?;
    output_file
        .flush()
        .context("failed to flush terminal pty combined output log")?;
    Ok(total)
}

pub(super) fn is_pty_eof_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
    ) || error.raw_os_error() == Some(5)
}

pub(super) fn spawn_capture_task<R>(
    reader: Option<R>,
    stream_path: PathBuf,
    output_file: Arc<Mutex<File>>,
) -> JoinHandle<Result<u64>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(capture_stream(reader, stream_path, output_file))
}

pub(super) async fn capture_stream<R>(
    mut reader: Option<R>,
    stream_path: PathBuf,
    output_file: Arc<Mutex<File>>,
) -> Result<u64>
where
    R: AsyncRead + Unpin,
{
    let mut stream_file = open_append_file(&stream_path).await?;
    let Some(reader) = reader.as_mut() else {
        return Ok(0);
    };
    let mut total = 0u64;
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await.with_context(|| {
            format!(
                "failed to read terminal stream for {}",
                stream_path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        stream_file
            .write_all(&buffer[..read])
            .await
            .with_context(|| format!("failed to write {}", stream_path.display()))?;
        let mut combined = output_file.lock().await;
        combined
            .write_all(&buffer[..read])
            .await
            .context("failed to write terminal combined output log")?;
        total += read as u64;
    }
    stream_file
        .flush()
        .await
        .with_context(|| format!("failed to flush {}", stream_path.display()))?;
    output_file
        .lock()
        .await
        .flush()
        .await
        .context("failed to flush terminal combined output log")?;
    Ok(total)
}

pub(super) async fn join_capture_task(task: JoinHandle<Result<u64>>) -> Result<u64> {
    match task.await {
        Ok(result) => result,
        Err(error) => Err(anyhow!("terminal capture task failed: {error}")),
    }
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
