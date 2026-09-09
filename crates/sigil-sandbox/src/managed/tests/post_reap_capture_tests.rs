use super::*;
use std::process::{Command, Stdio};

#[test]
fn post_reap_capture_preserves_real_output() -> io::Result<()> {
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf actual-output"]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "echo actual-output"]);
        command
    };
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child.wait()?;
    let result = bounded_post_reap_read(&mut child.stdout.take().expect("owned stdout"), 4096)?;
    assert!(String::from_utf8_lossy(&result.summary.retained_payload).contains("actual-output"));
    assert!(!result.summary.truncated);
    assert_eq!(result.summary.source, ManagedOutputSourceV1::Complete);
    Ok(())
}

#[cfg(unix)]
#[test]
fn post_reap_capture_does_not_wait_for_an_inherited_writer() -> io::Result<()> {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "sleep 1 & printf inherited-output; exit 0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child.wait()?;
    let started = Instant::now();
    let result = bounded_post_reap_read(&mut child.stdout.take().expect("owned stdout"), 4096)?;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(result.summary.retained_payload, b"inherited-output");
    assert!(!result.summary.truncated);
    assert_eq!(result.summary.source, ManagedOutputSourceV1::Incomplete);
    Ok(())
}

#[test]
fn post_reap_capture_caps_a_continuously_available_source() -> io::Result<()> {
    let mut reads = 0;
    let result = drain_available(16, |buffer| {
        reads += 1;
        buffer.fill(b'x');
        Ok(AvailableRead::Bytes(buffer.len()))
    })?;
    assert_eq!(reads, 1);
    assert_eq!(result.summary.observed_bytes, 17);
    assert_eq!(result.summary.retained_payload, vec![b'x'; 16]);
    assert!(result.summary.truncated);
    Ok(())
}
