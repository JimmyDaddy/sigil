//! Cancellation-safe stdin writes: one bounded owned write at a time, always reclaimed by the
//! handle. Dropping a caller future cannot detach a writer or hold the executor in a blocking IO.

use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

pub(super) struct PersistentInput {
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    pending: Mutex<Option<JoinHandle<io::Result<()>>>>,
    stopped: Arc<AtomicBool>,
}

impl PersistentInput {
    pub(super) fn new(writer: Option<Box<dyn Write + Send>>) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
            pending: Mutex::new(None),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) async fn write(&self, payload: Vec<u8>) -> io::Result<()> {
        // The kernel input type is intentionally small but cannot itself enforce a dynamic bound.
        // A single input owner never accepts a second job before the first one is joined.
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| io::Error::other("input owner poisoned"))?;
            if pending.is_some() || self.stopped.load(Ordering::SeqCst) {
                return Err(io::Error::other("input owner is not accepting writes"));
            }
            let writer = Arc::clone(&self.writer);
            let stopped = Arc::clone(&self.stopped);
            *pending = Some(
                std::thread::Builder::new()
                    .name("sigil-process-input".to_owned())
                    .spawn(move || {
                        let mut guard = writer
                            .lock()
                            .map_err(|_| io::Error::other("input writer poisoned"))?;
                        let writer = guard
                            .as_mut()
                            .ok_or_else(|| io::Error::other("stdin is closed"))?;
                        let mut offset = 0;
                        while offset < payload.len() {
                            if stopped.load(Ordering::SeqCst) {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "stdin write cancelled",
                                ));
                            }
                            match writer.write(&payload[offset..payload.len().min(offset + 4096)]) {
                                Ok(0) => {
                                    return Err(io::Error::new(
                                        io::ErrorKind::WriteZero,
                                        "stdin closed during write",
                                    ));
                                }
                                Ok(written) => offset += written,
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                                    ) =>
                                {
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        Ok(())
                    })?,
            );
        }
        loop {
            let ready = self
                .pending
                .lock()
                .map_err(|_| io::Error::other("input owner poisoned"))?
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            if ready {
                return match self
                    .pending
                    .lock()
                    .map_err(|_| io::Error::other("input owner poisoned"))?
                    .take()
                {
                    Some(worker) => worker
                        .join()
                        .map_err(|_| io::Error::other("stdin writer panicked"))?,
                    None => Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "stdin write cancelled",
                    )),
                };
            }
            super::yield_between_child_probes().await;
        }
    }

    pub(super) fn interrupt(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        if let Some(worker) = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: this owned JoinHandle stays alive under the pending lock. Cancellation
            // targets only this writer's synchronous IO, never another execution's thread.
            unsafe {
                windows_sys::Win32::System::IO::CancelSynchronousIo(worker.as_raw_handle());
            }
        }
    }

    pub(super) fn close_and_join(&self) -> io::Result<()> {
        self.interrupt();
        // Unix writes are nonblocking; Windows cancels the exact owned thread's synchronous IO.
        // Repeat cancellation to cover the race between the stop check and entering WriteFile.
        loop {
            let finished = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            if finished {
                break;
            }
            self.interrupt();
            std::thread::sleep(Duration::from_millis(1));
        }
        let joined = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .map(|worker| worker.join());
        self.writer
            .lock()
            .map_err(|_| io::Error::other("input writer poisoned"))?
            .take();
        if joined.is_some_and(|result| result.is_err()) {
            return Err(io::Error::other("stdin writer panicked"));
        }
        Ok(())
    }
}

impl Drop for PersistentInput {
    fn drop(&mut self) {
        let _ = self.close_and_join();
    }
}

#[cfg(unix)]
pub(super) fn make_nonblocking(descriptor: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: the persistent IO owner holds this live pipe/master for the entire write lifetime.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same exclusively owned descriptor remains live; its drain handles WouldBlock.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
