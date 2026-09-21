use super::*;

#[test]
fn elapsed_tracking_requires_an_actual_running_execution() {
    let entry = |status: &str, started: serde_json::Value| TimelineEntry {
        role: TimelineRole::Tool,
        text: serde_json::json!({"tool_name":"exec_command", "status": status,
            "metadata":{"details":{"started_at_ms":started,"status":status}}})
        .to_string(),
    };
    assert!(!has_running_elapsed(&entry(
        "pending",
        serde_json::Value::Null
    )));
    assert!(!has_running_elapsed(&entry(
        "approval",
        serde_json::json!(10)
    )));
    assert!(has_running_elapsed(&entry(
        "running",
        serde_json::json!(10)
    )));
    assert!(!has_running_elapsed(&entry(
        "exited",
        serde_json::json!(10)
    )));
}
