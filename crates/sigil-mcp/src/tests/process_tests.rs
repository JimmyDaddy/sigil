use std::time::Instant;

use anyhow::Result;

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn bounded_cleanup_command_status_enforces_timeout_and_kill_on_drop() -> Result<()> {
    let mut success = Command::new("sh");
    success.args(["-c", "exit 0"]);
    let status = bounded_cleanup_command_status(success, "successful cleanup probe").await?;
    assert!(status.success());

    let mut stalled = Command::new("sh");
    stalled.args(["-c", "exec sleep 30"]);
    let started = Instant::now();
    let error = bounded_cleanup_command_status(stalled, "stalled cleanup probe")
        .await
        .expect_err("stalled cleanup command must time out");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        error
            .to_string()
            .contains("bounded cleanup command timeout")
    );
    Ok(())
}
