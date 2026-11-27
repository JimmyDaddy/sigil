use std::{fs, process::Command};

use sigil_runtime::support::{DOCTOR_SUPPORT_SCHEMA_VERSION, DoctorSupportReportV1};

mod common;

fn test_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sigil-doctor-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("doctor process root should create");
    root
}

#[test]
fn doctor_json_process_emits_one_parseable_redacted_document() {
    let root = test_root("json");
    let config = root.join("missing-sigil.toml");
    let canaries = [
        "TERM-CANARY-process-private",
        "PROGRAM-CANARY-process-private",
        "VERSION-CANARY-process-private",
        "PROFILE-CANARY-process-private",
    ];
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
    common::isolated_child_environment(&root)
        .expect("doctor child environment should create")
        .apply_to_command(&mut command);
    let output = command
        .args([
            "--config",
            config.to_str().expect("test config is UTF-8"),
            "doctor",
            "--output",
            "json",
        ])
        .env("TERM", canaries[0])
        .env("TERM_PROGRAM", canaries[1])
        .env("TERM_PROGRAM_VERSION", canaries[2])
        .env("ITERM_PROFILE", canaries[3])
        .output()
        .expect("doctor process should start");
    assert!(
        output.status.success(),
        "doctor JSON failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: DoctorSupportReportV1 =
        serde_json::from_slice(&output.stdout).expect("stdout should be one JSON document");
    assert_eq!(report.schema_version, DOCTOR_SUPPORT_SCHEMA_VERSION);
    // Missing config reports one error (`config:load`); an uninitialized bootstrap realm is
    // reported as a warning without fabricating a corrupt-manifest blocker.
    assert_eq!(report.summary.error, 1);
    assert!(report.cutover.blockers.is_empty());
    assert!(report.privacy.review_before_sharing);
    let stdout = String::from_utf8(output.stdout).expect("doctor JSON should be UTF-8");
    for canary in canaries {
        assert!(!stdout.contains(canary), "terminal canary leaked: {canary}");
    }
    assert!(!stdout.contains(&root.display().to_string()));
    assert!(output.stderr.is_empty());
    fs::remove_dir_all(root).expect("doctor process root should clean up");
}

#[test]
fn doctor_text_process_remains_the_default() {
    let root = test_root("text");
    let config = root.join("missing-sigil.toml");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
    common::isolated_child_environment(&root)
        .expect("doctor child environment should create")
        .apply_to_command(&mut command);
    let output = command
        .args([
            "--config",
            config.to_str().expect("test config is UTF-8"),
            "doctor",
        ])
        .output()
        .expect("doctor process should start");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("doctor text should be UTF-8");
    assert!(stdout.starts_with("Sigil doctor\n"));
    assert!(stdout.contains("summary: error"));
    fs::remove_dir_all(root).expect("doctor process root should clean up");
}

#[test]
fn doctor_json_process_reports_ready_after_real_current_schema_boot() {
    let root = test_root("booted-json");
    let workspace = root.join("workspace");
    let state = root.join("state");
    let cache = root.join("cache");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(&cache).expect("cache");
    let config = root.join("sigil.toml");
    let state_toml = state.to_string_lossy().replace('\\', "/");
    let cache_toml = cache.to_string_lossy().replace('\\', "/");
    fs::write(
        &config,
        format!(
            "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\ncredential_store = \"file\"\nstate_root = \"{state_toml}\"\ncache_root = \"{cache_toml}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n"
        ),
    )
    .expect("config");

    let env_fixture =
        common::isolated_child_environment(&root).expect("doctor child environment should create");

    // Pre-boot doctor in isolated HOME must stay read-only and report no corrupt blocker.
    let mut pre_cmd = Command::new(env!("CARGO_BIN_EXE_sigil"));
    env_fixture.apply_to_command(&mut pre_cmd);
    let pre_output = pre_cmd
        .current_dir(&workspace)
        .args([
            "--config",
            config.to_str().expect("UTF-8 config"),
            "doctor",
            "--output",
            "json",
        ])
        .output()
        .expect("pre-boot doctor should run");
    assert!(pre_output.status.success());
    let pre_report: DoctorSupportReportV1 =
        serde_json::from_slice(&pre_output.stdout).expect("pre-boot JSON");
    assert!(pre_report.cutover.blockers.is_empty());
    assert!(
        !root.join("home/.sigil").exists(),
        "pre-boot doctor must not create ~/.sigil"
    );

    // Run real current-schema boot under the exact same isolated HOME, then run doctor process.
    let mut boot_cmd = Command::new(env!("CARGO_BIN_EXE_sigil"));
    env_fixture.apply_to_command(&mut boot_cmd);
    let boot_output = boot_cmd
        .current_dir(&workspace)
        .args([
            "--config",
            config.to_str().expect("UTF-8 config"),
            "run",
            "--output",
            "json",
            "ping",
        ])
        .output()
        .expect("booted run invocation should start");
    // The provider endpoint 127.0.0.1:1 is intentionally offline, so `run` fails after
    // completing the real current-schema boot transaction and publishing the manifest.
    let _ = boot_output.status;

    let mut post_cmd = Command::new(env!("CARGO_BIN_EXE_sigil"));
    env_fixture.apply_to_command(&mut post_cmd);
    let post_output = post_cmd
        .current_dir(&workspace)
        .args([
            "--config",
            config.to_str().expect("UTF-8 config"),
            "doctor",
            "--output",
            "json",
        ])
        .output()
        .expect("post-boot doctor should run");
    assert!(post_output.status.success());
    let post_report: DoctorSupportReportV1 =
        serde_json::from_slice(&post_output.stdout).expect("post-boot JSON");
    assert_eq!(
        post_report.cutover.epoch,
        sigil_kernel::cutover_manifest::CutoverSurfaceEpochV1::NewCurrentSchema
    );
    assert_eq!(
        post_report.cutover.authority,
        sigil_kernel::cutover_manifest::CutoverAuthorityStateV1::Ready
    );
    assert!(post_report.cutover.blockers.is_empty());
    assert_eq!(
        post_report.summary.error, 0,
        "post-boot doctor checks: {:?}",
        post_report.checks
    );
    fs::remove_dir_all(root).expect("doctor process root should clean up");
}
