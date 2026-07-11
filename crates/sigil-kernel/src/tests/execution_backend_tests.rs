use crate::{
    EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionNetworkReceipt, ExecutionOutputReceipt, ExecutionOutputStream, ExecutionReceipt,
    ExecutionSandboxProfile, ExecutionStreamCapture, ExecutionTerminationCause,
    ExtensionProcessLaunchErrorCode, ExtensionProcessNetworkAdmission, NetworkEffect,
    NetworkPolicy, ProcessEnvironmentPolicy,
    validate_extension_process_isolation_with_network_policy,
    validate_extension_process_network_admission,
    validate_extension_process_network_receipt_with_policy,
};

#[test]
fn legacy_execution_receipt_derives_accurate_output_evidence() {
    let mut legacy = serde_json::to_value(ExecutionReceipt {
        backend: ExecutionBackendKind::Local,
        capabilities: ExecutionBackendCapabilities::default(),
        network: Default::default(),
        resources: Default::default(),
        environment_policy: ProcessEnvironmentPolicy::InheritParent,
        exit_code: None,
        stdout: b"a\nb\n".to_vec(),
        stderr: b"err".to_vec(),
        output: ExecutionOutputReceipt::default(),
        timed_out: true,
    })
    .expect("execution receipt should serialize");
    legacy
        .as_object_mut()
        .expect("execution receipt should serialize as an object")
        .remove("output");
    let receipt = serde_json::from_value::<ExecutionReceipt>(legacy)
        .expect("legacy execution receipt should deserialize");

    assert!(!receipt.output.is_recorded());
    let output = receipt.effective_output();
    assert!(output.is_recorded());
    assert_eq!(output.stdout.total_bytes, 4);
    assert_eq!(output.stdout.total_lines, 2);
    assert_eq!(output.stderr.total_bytes, 3);
    assert_eq!(output.stderr.total_lines, 1);
    assert_eq!(output.combined_total_bytes, 7);
    assert_eq!(output.termination, ExecutionTerminationCause::TimedOut);
}

#[test]
fn extension_network_deny_requires_proven_process_tree_isolation() {
    let error = validate_extension_process_isolation_with_network_policy(
        ExecutionSandboxProfile::Unconfined,
        Some(NetworkEffect::Unknown),
        NetworkPolicy::Deny,
        ExecutionBackendCapabilities::default(),
        &ExecutionNetworkReceipt::unknown("local backend"),
        "plugin-hook",
    )
    .expect_err("unknown extension network effect must fail closed without isolation");
    assert_eq!(
        error.code,
        ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable
    );

    let isolated = ExecutionBackendCapabilities {
        network_isolation: true,
        process_isolation: true,
        ..ExecutionBackendCapabilities::default()
    };
    validate_extension_process_isolation_with_network_policy(
        ExecutionSandboxProfile::Unconfined,
        Some(NetworkEffect::Unknown),
        NetworkPolicy::Deny,
        isolated,
        &ExecutionNetworkReceipt::denied("isolated process tree"),
        "plugin-hook",
    )
    .expect("denied launch receipt with process-tree isolation should satisfy policy");
}

#[test]
fn extension_network_allow_and_no_effect_do_not_invent_isolation_requirements() {
    for (effect, policy) in [
        (Some(NetworkEffect::Unknown), NetworkPolicy::Allow),
        (None, NetworkPolicy::Deny),
    ] {
        validate_extension_process_isolation_with_network_policy(
            ExecutionSandboxProfile::Unconfined,
            effect,
            policy,
            ExecutionBackendCapabilities::default(),
            &ExecutionNetworkReceipt::unknown("local backend"),
            "extension",
        )
        .expect("independent policy should not require isolation for this case");
    }
}

#[test]
fn extension_network_deny_revalidates_completed_backend_receipt() {
    let error = validate_extension_process_network_receipt_with_policy(
        ExecutionSandboxProfile::Unconfined,
        Some(NetworkEffect::Unknown),
        NetworkPolicy::Deny,
        &ExecutionNetworkReceipt::allowed("unexpected network allowance"),
        "mcp-server",
    )
    .expect_err("deny policy requires a denied backend receipt");
    assert_eq!(
        error.code,
        ExtensionProcessLaunchErrorCode::BackendReceiptInvalid
    );
}

#[test]
fn extension_network_admission_matrix_is_fail_closed_and_profile_aware() {
    assert_eq!(
        ExtensionProcessNetworkAdmission::default(),
        ExtensionProcessNetworkAdmission::new(NetworkPolicy::Allow, false)
    );

    for effect in [
        NetworkEffect::Read,
        NetworkEffect::Mutate,
        NetworkEffect::Unknown,
    ] {
        let error = validate_extension_process_network_admission(
            ExecutionSandboxProfile::Unconfined,
            Some(effect),
            ExtensionProcessNetworkAdmission::new(NetworkPolicy::Ask, false),
            ExecutionBackendCapabilities::default(),
            &ExecutionNetworkReceipt::unknown("local backend"),
            "extension",
        )
        .expect_err("ask admission without explicit approval must fail before spawn");
        assert_eq!(
            error.code,
            ExtensionProcessLaunchErrorCode::NetworkApprovalRequired
        );

        validate_extension_process_network_admission(
            ExecutionSandboxProfile::Unconfined,
            Some(effect),
            ExtensionProcessNetworkAdmission::new(NetworkPolicy::Ask, true),
            ExecutionBackendCapabilities::default(),
            &ExecutionNetworkReceipt::unknown("local backend"),
            "extension",
        )
        .expect("explicit approval should satisfy the independent ask admission");
    }

    validate_extension_process_network_admission(
        ExecutionSandboxProfile::Unconfined,
        None,
        ExtensionProcessNetworkAdmission::new(NetworkPolicy::Ask, false),
        ExecutionBackendCapabilities::default(),
        &ExecutionNetworkReceipt::unknown("local backend"),
        "extension-without-network-effect",
    )
    .expect("no declared network effect should add no independent network gate");

    let profile_error = validate_extension_process_network_admission(
        ExecutionSandboxProfile::BuildOffline,
        None,
        ExtensionProcessNetworkAdmission::new(NetworkPolicy::Allow, false),
        ExecutionBackendCapabilities::default(),
        &ExecutionNetworkReceipt::unknown("local backend"),
        "offline-extension",
    )
    .expect_err("profile validation must still run when no network effect is declared");
    assert_eq!(
        profile_error.code,
        ExtensionProcessLaunchErrorCode::ProcessIsolationUnavailable
    );
}

#[test]
fn bounded_execution_output_evidence_roundtrips() {
    let output = ExecutionOutputReceipt {
        schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
        stdout: ExecutionStreamCapture {
            total_bytes: 20,
            returned_bytes: 8,
            omitted_bytes: 12,
            retained_head_bytes: 4,
            retained_tail_bytes: 4,
            retained_limit_bytes: 8,
            hard_limit_bytes: 16,
            total_lines: 3,
            truncated: true,
        },
        stderr: ExecutionStreamCapture::default(),
        combined_total_bytes: 20,
        combined_hard_limit_bytes: 32,
        termination: ExecutionTerminationCause::OutputLimit {
            stream: ExecutionOutputStream::Stdout,
            limit_bytes: 16,
            observed_bytes: 20,
        },
    };

    let encoded = serde_json::to_value(&output).expect("output evidence should serialize");
    assert_eq!(encoded["termination"]["kind"], "output_limit");
    assert_eq!(encoded["termination"]["stream"], "stdout");
    let decoded = serde_json::from_value::<ExecutionOutputReceipt>(encoded)
        .expect("output evidence should deserialize");
    assert_eq!(decoded, output);
}

#[test]
fn future_execution_output_schema_preserves_known_terminal_evidence() {
    let output = ExecutionOutputReceipt {
        schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
        stdout: ExecutionStreamCapture {
            total_bytes: 42,
            returned_bytes: 10,
            omitted_bytes: 32,
            retained_head_bytes: 5,
            retained_tail_bytes: 5,
            retained_limit_bytes: 10,
            hard_limit_bytes: 40,
            total_lines: 7,
            truncated: true,
        },
        stderr: ExecutionStreamCapture::default(),
        combined_total_bytes: 42,
        combined_hard_limit_bytes: 80,
        termination: ExecutionTerminationCause::ReaderFailed {
            stream: ExecutionOutputStream::Stdout,
            reason: "future reader evidence".to_owned(),
        },
    };
    let mut encoded = serde_json::to_value(output).expect("output evidence should serialize");
    encoded["schema_version"] = serde_json::json!(2);
    encoded["future_evidence"] = serde_json::json!({ "new_counter": 9 });
    let decoded = serde_json::from_value::<ExecutionOutputReceipt>(encoded)
        .expect("future additive output evidence should deserialize");

    assert!(decoded.is_recorded());
    assert!(!decoded.uses_current_schema());
    let receipt = ExecutionReceipt {
        backend: ExecutionBackendKind::Local,
        capabilities: ExecutionBackendCapabilities::default(),
        network: Default::default(),
        resources: Default::default(),
        environment_policy: ProcessEnvironmentPolicy::InheritParent,
        exit_code: None,
        stdout: b"legacy bytes must not replace future evidence".to_vec(),
        stderr: Vec::new(),
        output: decoded.clone(),
        timed_out: false,
    };

    let effective = receipt.effective_output();
    assert_eq!(effective, decoded);
    assert_eq!(effective.schema_version, 2);
    assert_eq!(effective.stdout.total_bytes, 42);
    assert_eq!(effective.combined_total_bytes, 42);
    assert_eq!(
        effective.termination,
        ExecutionTerminationCause::ReaderFailed {
            stream: ExecutionOutputStream::Stdout,
            reason: "future reader evidence".to_owned(),
        }
    );
}
