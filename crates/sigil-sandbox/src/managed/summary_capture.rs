//! Sole-owner, nonblocking one-shot pipe readers with explicit EOF evidence and joined cleanup.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{
    BoundedReadOutcome, CapState, ManagedOutputCaptureSinkV1, ManagedOutputSourceV1,
    ManagedProcessOutputChannelV1,
};

const POST_LEADER_IDLE_BUDGET: Duration = Duration::from_millis(50);
const POST_LEADER_TOTAL_BUDGET: Duration = Duration::from_secs(1);
const PIPE_POLL_INTERVAL: Duration = Duration::from_millis(1);

pub(super) enum AvailableRead {
    Bytes(usize),
    Eof,
    Pending,
}

pub(super) trait CapturePipe: Read + Send + 'static {
    fn prepare(&self) -> io::Result<()>;
    fn read_available(&mut self, buffer: &mut [u8]) -> io::Result<AvailableRead>;
}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd + Send + 'static> CapturePipe for T {
    fn prepare(&self) -> io::Result<()> {
        let descriptor = self.as_raw_fd();
        // SAFETY: the capture owner holds the sole reader and keeps its descriptor live until
        // the reader thread joins. No other thread changes the descriptor's status flags.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: these flags belong to the same live, exclusively owned pipe descriptor.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn read_available(&mut self, buffer: &mut [u8]) -> io::Result<AvailableRead> {
        match self.read(buffer) {
            Ok(0) => Ok(AvailableRead::Eof),
            Ok(count) => Ok(AvailableRead::Bytes(count)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(AvailableRead::Pending),
            Err(error) => Err(error),
        }
    }
}

#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle + Send + 'static> CapturePipe for T {
    fn prepare(&self) -> io::Result<()> {
        Ok(())
    }

    fn read_available(&mut self, buffer: &mut [u8]) -> io::Result<AvailableRead> {
        use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;

        let mut available = 0_u32;
        // SAFETY: the capture thread is the sole reader of this synchronous pipe. Peek does
        // not consume bytes; the subsequent read requests only bytes already available.
        let ok = unsafe {
            PeekNamedPipe(
                self.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(AvailableRead::Eof),
                _ => Err(error),
            };
        }
        if available == 0 {
            return Ok(AvailableRead::Pending);
        }
        let limit = buffer.len().min(available as usize);
        match self.read(&mut buffer[..limit]) {
            Ok(0) => Ok(AvailableRead::Eof),
            Ok(count) => Ok(AvailableRead::Bytes(count)),
            Err(error) => Err(error),
        }
    }
}

#[derive(Default)]
struct CaptureControl {
    leader_finished: AtomicBool,
    stop: AtomicBool,
}

pub(super) struct SummaryCapture {
    reader: Option<JoinHandle<()>>,
    state: Arc<Mutex<CapState>>,
    control: Arc<CaptureControl>,
}

impl SummaryCapture {
    pub(super) fn mark_leader_finished(&self) {
        self.control.leader_finished.store(true, Ordering::Release);
    }

    pub(super) async fn finish(mut self) -> BoundedReadOutcome {
        while self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_finished())
        {
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::time::sleep(PIPE_POLL_INTERVAL).await;
            } else {
                std::thread::sleep(PIPE_POLL_INTERVAL);
            }
        }
        self.join_reader();
        BoundedReadOutcome {
            summary: self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .summary(),
        }
    }

    fn join_reader(&mut self) {
        if let Some(reader) = self.reader.take()
            && reader.join().is_err()
        {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .source = ManagedOutputSourceV1::ReadFailed;
        }
    }
}

impl Drop for SummaryCapture {
    fn drop(&mut self) {
        // Early-return and cancelled-future paths retain reader ownership too. Reads and sink
        // enqueue are nonblocking, so this stop is observed within one bounded poll interval.
        self.control.stop.store(true, Ordering::Release);
        self.join_reader();
    }
}

pub(super) fn spawn_summary_capture(
    mut pipe: impl CapturePipe,
    cap: u64,
    channel: ManagedProcessOutputChannelV1,
    sink: Option<Arc<dyn ManagedOutputCaptureSinkV1>>,
) -> io::Result<SummaryCapture> {
    pipe.prepare()?;
    spawn_capture_reader(
        move |buffer| pipe.read_available(buffer),
        cap,
        channel,
        sink,
    )
}

pub(super) fn spawn_capture_reader(
    mut read: impl FnMut(&mut [u8]) -> io::Result<AvailableRead> + Send + 'static,
    cap: u64,
    channel: ManagedProcessOutputChannelV1,
    mut sink: Option<Arc<dyn ManagedOutputCaptureSinkV1>>,
) -> io::Result<SummaryCapture> {
    let state = Arc::new(Mutex::new(CapState::new(cap)));
    let state_for_reader = Arc::clone(&state);
    let control = Arc::new(CaptureControl::default());
    let control_for_reader = Arc::clone(&control);
    let reader = std::thread::Builder::new()
        .name("sigil-output-reader".to_owned())
        .spawn(move || {
            let mut chunk = [0_u8; 4096];
            let mut post_leader_started = None;
            let mut idle_started = None;
            let source = loop {
                if control_for_reader.stop.load(Ordering::Acquire) {
                    break ManagedOutputSourceV1::Incomplete;
                }
                if control_for_reader.leader_finished.load(Ordering::Acquire) {
                    let started = post_leader_started.get_or_insert_with(Instant::now);
                    if started.elapsed() >= POST_LEADER_TOTAL_BUDGET {
                        break ManagedOutputSourceV1::Incomplete;
                    }
                }
                match read(&mut chunk) {
                    Ok(AvailableRead::Eof) => break ManagedOutputSourceV1::Complete,
                    Ok(AvailableRead::Bytes(count)) => {
                        idle_started = None;
                        state_for_reader
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(&chunk[..count]);
                        // The host latches queue/storage failures. Disable this channel's sink
                        // after rejection, while continuing to observe and drain the pipe.
                        if sink
                            .as_ref()
                            .is_some_and(|sink| sink.write_chunk(channel, &chunk[..count]).is_err())
                        {
                            sink = None;
                        }
                    }
                    Ok(AvailableRead::Pending) => {
                        if post_leader_started.is_some() {
                            let started = idle_started.get_or_insert_with(Instant::now);
                            if started.elapsed() >= POST_LEADER_IDLE_BUDGET {
                                break ManagedOutputSourceV1::Incomplete;
                            }
                        }
                        std::thread::sleep(PIPE_POLL_INTERVAL);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break ManagedOutputSourceV1::ReadFailed,
                }
            };
            state_for_reader
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .source = source;
        })?;
    Ok(SummaryCapture {
        reader: Some(reader),
        state,
        control,
    })
}

#[cfg(test)]
#[path = "tests/summary_capture_tests.rs"]
mod tests;
