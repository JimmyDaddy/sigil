use serde_json::{Value, json};
use sigil_kernel::{PublicRunEvent, PublicRunEventKind};

use super::{
    MACHINE_PROTOCOL_VERSION, MachineError, MachineErrorCode, MachineExitCode, MachineRecord,
    MachineRunResult, MachineRunStatus,
};

#[test]
fn event_record_serializes_with_stable_v1_envelope() {
    let record = MachineRecord::event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "inspect workspace".to_owned(),
        },
    ));

    let value = serde_json::to_value(record).expect("machine event should serialize");

    assert_eq!(
        value,
        json!({
            "record_type": "event",
            "protocol_version": 1,
            "event": {
                "schema_version": 1,
                "session_id": "session-1",
                "run_id": "run-1",
                "sequence": 1,
                "event": {
                    "type": "run_started",
                    "prompt": "inspect workspace"
                }
            }
        })
    );
}

#[test]
fn result_record_has_one_stable_terminal_payload() {
    let record = MachineRecord::result(MachineRunResult {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        status: MachineRunStatus::Succeeded,
        final_text: "done".to_owned(),
        session_log_path: "/tmp/session-1.jsonl".to_owned(),
    });

    let encoded = serde_json::to_string(&record).expect("machine result should serialize");
    let value: Value = serde_json::from_str(&encoded).expect("machine result should parse");
    let roundtripped: MachineRecord =
        serde_json::from_value(value.clone()).expect("machine result should deserialize");

    assert_eq!(value["record_type"], "result");
    assert_eq!(value["protocol_version"], MACHINE_PROTOCOL_VERSION);
    assert_eq!(value["result"]["status"], "succeeded");
    assert_eq!(value["result"]["final_text"], "done");
    assert_eq!(value["result"]["session_log_path"], "/tmp/session-1.jsonl");
    assert!(matches!(roundtripped, MachineRecord::Result { .. }));
}

#[test]
fn error_record_uses_stable_code_without_provider_payload() {
    let record = MachineRecord::error(MachineError::new(
        MachineErrorCode::ConfigurationInvalid,
        "required API key environment variable is missing",
        false,
    ));

    let value = serde_json::to_value(record).expect("machine error should serialize");

    assert_eq!(value["record_type"], "error");
    assert_eq!(value["protocol_version"], 1);
    assert_eq!(value["error"]["code"], "configuration_invalid");
    assert_eq!(value["error"]["retryable"], false);
    assert_eq!(
        value["error"]["message"],
        "required API key environment variable is missing"
    );
    assert!(value.get("provider_payload").is_none());
}

#[test]
fn exit_codes_are_stable_for_terminal_status_and_error_class() {
    assert_eq!(MachineExitCode::Success.as_i32(), 0);
    assert_eq!(MachineExitCode::ExecutionFailed.as_i32(), 1);
    assert_eq!(MachineExitCode::InvalidInput.as_i32(), 2);
    assert_eq!(MachineExitCode::Cancelled.as_i32(), 130);

    assert_eq!(
        MachineExitCode::for_status(MachineRunStatus::Succeeded),
        MachineExitCode::Success
    );
    assert_eq!(
        MachineExitCode::for_status(MachineRunStatus::Failed),
        MachineExitCode::ExecutionFailed
    );
    assert_eq!(
        MachineExitCode::for_status(MachineRunStatus::Cancelled),
        MachineExitCode::Cancelled
    );
    assert_eq!(
        MachineExitCode::for_error(MachineErrorCode::InvalidInvocation),
        MachineExitCode::InvalidInput
    );
    assert_eq!(
        MachineExitCode::for_error(MachineErrorCode::ConfigurationInvalid),
        MachineExitCode::InvalidInput
    );
    assert_eq!(
        MachineExitCode::for_error(MachineErrorCode::ExecutionFailed),
        MachineExitCode::ExecutionFailed
    );
    assert_eq!(
        MachineExitCode::for_error(MachineErrorCode::Cancelled),
        MachineExitCode::Cancelled
    );
    assert_eq!(
        MachineExitCode::for_error(MachineErrorCode::Internal),
        MachineExitCode::ExecutionFailed
    );
}
