use super::execution::*;
use crate::{AgentRunOutcome, ToolErrorKind};

#[test]
fn recovered_denial_is_history_but_active_effect_is_not() {
    let mut outcome = AgentRunOutcome {
        approval_denials: 1,
        ..AgentRunOutcome::default()
    };
    assert_eq!(
        outcome.execution_disposition(""),
        ExecutionDisposition::Blocked
    );
    assert_eq!(
        outcome.execution_disposition("delivered by safe alternative"),
        ExecutionDisposition::Completed
    );
    outcome.tool_errors.push(crate::ToolError {
        kind: ToolErrorKind::EffectReconciliationRequired,
        message: "effect awaiting owner reconciliation".to_owned(),
        retryable: true,
        details: serde_json::json!({"active": true}),
    });
    assert_eq!(
        outcome.execution_disposition("done"),
        ExecutionDisposition::Blocked
    );
    outcome.tool_errors[0].details = serde_json::json!({"active": false});
    assert_eq!(
        outcome.execution_disposition("done"),
        ExecutionDisposition::Completed
    );
}

#[test]
fn missing_final_report_is_not_success() {
    assert_eq!(
        AgentRunOutcome::default().execution_disposition(" "),
        ExecutionDisposition::Blocked
    );
}
