use std::{
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use super::*;

const LIVE_CHILD_FIXTURE: &str = "identity::tests::owned_child_waits_for_lifecycle_probe";
const EXITED_CHILD_FIXTURE: &str = "identity::tests::owned_child_exits_for_lifecycle_probe";

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn process_birth_identity_for_current_host_is_stable() {
    let first = observe_current_process_identity().expect("current process identity");
    let second = observe_current_process_identity().expect("current process identity recheck");

    assert_eq!(first.process_id(), std::process::id());
    assert_eq!(first, second);
    assert_eq!(
        first.birth_identity_fingerprint(),
        second.birth_identity_fingerprint()
    );
}

#[test]
fn zero_process_identifier_is_rejected_before_platform_probe() {
    assert!(matches!(
        observe_process_identity(0),
        Err(ProcessIdentityObservationErrorV1::InvalidProcessId)
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn platform_birth_identity_distinguishes_an_owned_live_child() -> anyhow::Result<()> {
    let mut child = spawn_identity_fixture(LIVE_CHILD_FIXTURE)?;
    let process_id = child.id();
    let identity = observe_owned_live_child(process_id, &mut child)?;
    assert_eq!(identity.process_id(), process_id);

    child.kill().context("terminate owned live-child fixture")?;
    child.wait().context("reap owned live-child fixture")?;
    assert!(matches!(
        observe_process_identity(process_id),
        Err(ProcessIdentityObservationErrorV1::Absent)
            | Err(ProcessIdentityObservationErrorV1::NotLive(_))
    ));
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn platform_birth_identity_never_reports_an_owned_exited_child_as_live() -> anyhow::Result<()> {
    let mut child = spawn_identity_fixture(EXITED_CHILD_FIXTURE)?;
    let process_id = child.id();
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        match observe_process_identity(process_id) {
            Err(ProcessIdentityObservationErrorV1::NotLive(_)) => {
                child.wait().context("reap owned exited-child fixture")?;
                return Ok(());
            }
            // macOS can remove an exited child from proc_pidinfo before its Rust parent calls
            // wait. This is accepted only after the owned-child handle itself confirms exit;
            // absence remains an observation result, never terminal/quiescence evidence.
            Err(ProcessIdentityObservationErrorV1::Absent) => {
                let status = child
                    .try_wait()
                    .context("query owned exited-child fixture")?
                    .context("process became absent before the owned child exited")?;
                assert!(status.success(), "exited child fixture must succeed");
                return Ok(());
            }
            Ok(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(_) => {
                let _ = child.wait();
                bail!("exited owned child remained falsely observable as live")
            }
            Err(error) => {
                let _ = child.wait();
                bail!("exited owned child was not classified as not live: {error}")
            }
        }
    }
}

fn spawn_identity_fixture(test_name: &str) -> anyhow::Result<Child> {
    Command::new(std::env::current_exe().context("locate current test binary")?)
        // These fixtures are ignored in the parent suite and are run by a separately owned test
        // process, so each assertion observes an actual OS child without shell indirection.
        .args(["--exact", test_name, "--ignored"])
        .spawn()
        .with_context(|| format!("spawn owned identity fixture {test_name}"))
}

fn observe_owned_live_child(
    process_id: u32,
    child: &mut Child,
) -> anyhow::Result<ProcessIdentityV1> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match observe_process_identity(process_id) {
            Ok(identity) => return Ok(identity),
            // Darwin can briefly report SIDL while the just-spawned child reaches a schedulable
            // state. It is not live evidence yet, so retry rather than accepting it as Live.
            Err(ProcessIdentityObservationErrorV1::NotLive(_)) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("owned live child could not be observed as live: {error}")
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_stat_parser_uses_field_22_and_state_after_a_parenthesized_command_name() {
    let stat = "41 (worker) with ) delimiters) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 9876";

    let parsed = parse_linux_process_stat(stat, 41).expect("parse Linux stat fixture");
    assert_eq!(parsed.state, 'S');
    assert_eq!(parsed.start_time_ticks, 9876);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_zombie_state_is_never_reported_live() {
    assert!(matches!(
        ensure_linux_process_is_live('Z'),
        Err(ProcessIdentityObservationErrorV1::NotLive(_))
    ));
}

// The test harness invokes these only as child fixtures. They are deliberately ignored in the
// normal suite so they cannot leave a process behind when a developer filters ordinary tests.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
#[ignore]
fn owned_child_waits_for_lifecycle_probe() {
    thread::sleep(Duration::from_secs(30));
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
#[ignore]
fn owned_child_exits_for_lifecycle_probe() {}
