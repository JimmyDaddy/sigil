use std::{fs, process::Command};

use sigil_runtime::support::{DOCTOR_SUPPORT_SCHEMA_VERSION, DoctorSupportReportV1};

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
    let output = Command::new(env!("CARGO_BIN_EXE_sigil"))
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
    assert_eq!(report.summary.error, 1);
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
    let output = Command::new(env!("CARGO_BIN_EXE_sigil"))
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
