//! Bounded reads from the sole owned stdout/stderr reader after the direct child was reaped.
//! An inherited writer may remain open; no EOF or whole-tree quiescence is inferred from reap.

use std::io::{self, Read};
use std::time::{Duration, Instant};

use super::{BoundedOutputSummaryV1, BoundedReadOutcome, content_digest};

const MAX_DRAIN_TIME: Duration = Duration::from_millis(50);

#[cfg(unix)]
pub(super) fn bounded_post_reap_read(
    reader: &mut (impl Read + std::os::fd::AsRawFd),
    cap_bytes: u64,
) -> io::Result<BoundedReadOutcome> {
    let descriptor = reader.as_raw_fd();
    // SAFETY: the caller retains the sole owned reader throughout this bounded drain. No other
    // thread reads this descriptor, and the caller drops the pipe immediately afterwards.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor and flags refer to that same live, owned pipe reader.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    drain_available(cap_bytes, |buffer| match reader.read(buffer) {
        Ok(0) => Ok(AvailableRead::Eof),
        Ok(count) => Ok(AvailableRead::Bytes(count)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(AvailableRead::Pending),
        Err(error) => Err(error),
    })
}

#[cfg(windows)]
pub(super) fn bounded_post_reap_read(
    reader: &mut (impl Read + std::os::windows::io::AsRawHandle),
    cap_bytes: u64,
) -> io::Result<BoundedReadOutcome> {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    drain_available(cap_bytes, |buffer| {
        let mut available = 0_u32;
        // SAFETY: this is the sole owned anonymous-pipe reader, with no concurrent/pending read
        // on its synchronous handle. Peek does not consume data; the following read requests
        // only the bytes already observed, so an inherited writer cannot make it wait for EOF.
        let ok = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle(),
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
        match reader.read(&mut buffer[..limit]) {
            Ok(0) => Ok(AvailableRead::Eof),
            Ok(count) => Ok(AvailableRead::Bytes(count)),
            Err(error) => Err(error),
        }
    })
}

enum AvailableRead {
    Bytes(usize),
    Eof,
    Pending,
}

fn drain_available(
    cap_bytes: u64,
    mut read: impl FnMut(&mut [u8]) -> io::Result<AvailableRead>,
) -> io::Result<BoundedReadOutcome> {
    let started = Instant::now();
    let read_budget = cap_bytes.saturating_add(1);
    let mut observed = 0_u64;
    let mut retained = Vec::new();
    let mut chunk = [0_u8; 4096];
    let incomplete = loop {
        // Both bounds matter: a busy descendant can continuously refill the pipe and prevent
        // WouldBlock, while repeated interruptions must not defeat the time bound.
        if observed >= read_budget || started.elapsed() >= MAX_DRAIN_TIME {
            break true;
        }
        let limit = (read_budget - observed).min(chunk.len() as u64) as usize;
        match read(&mut chunk[..limit]) {
            Ok(AvailableRead::Bytes(count)) => {
                observed = observed.saturating_add(count as u64);
                let remaining = cap_bytes.saturating_sub(retained.len() as u64);
                retained.extend_from_slice(
                    &chunk[..count.min(remaining.min(usize::MAX as u64) as usize)],
                );
            }
            Ok(AvailableRead::Eof) => break false,
            Ok(AvailableRead::Pending) => break true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    };
    Ok(BoundedReadOutcome {
        summary: BoundedOutputSummaryV1 {
            observed_bytes: observed,
            retained_bytes: retained.len() as u64,
            content_digest: content_digest(&retained),
            retained_payload: retained,
            truncated: incomplete || observed > cap_bytes,
            artifact_ref: None,
        },
    })
}

#[cfg(test)]
#[path = "tests/post_reap_capture_tests.rs"]
mod tests;
