use std::path::Path;

use clap::Subcommand;
use serde::Serialize;
use sigil_kernel::{
    IntentDigest, IntentDropRequestV1, IntentId, IntentOperationId, IntentStackVersion,
    IntentVersionRef, RootConfig, resolve_workspace_root,
};
use sigil_runtime::{
    ApplicationIntentConfirmationSource, ApplicationIntentStackCommandOutputV1,
    ApplicationIntentStackCommandV1, ApplicationIntentStackErrorClass,
    LocalSessionLifecycleService, LocalSessionReopenBinding,
    execute_durable_application_intent_stack_command, machine_protocol::MachineExitCode,
    resolve_sigil_paths,
};

const INTENT_AUTOMATION_PROTOCOL_VERSION: u16 = 1;

/// Typed JSON automation commands for one exact durable Intent Stack.
#[derive(Debug, Subcommand)]
pub(crate) enum IntentCommand {
    /// Inspect the bounded current Intent Stack.
    Inspect,
    /// Build a fresh exact Drop preview for one immutable Intent version.
    DropPreview {
        #[arg(long)]
        intent_id: String,
        #[arg(long)]
        intent_version: u64,
    },
    /// Confirm and execute one exact digest-bound Drop preview.
    Drop {
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        stack_version: u64,
        #[arg(long)]
        preview_digest: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntentAutomationErrorCode {
    InvalidInvocation,
    ConfigurationInvalid,
    SessionNotFound,
    SessionAmbiguous,
    SessionUnavailable,
    IntentRequestInvalid,
    IntentStale,
    PermissionRequired,
    IntentConflict,
    IntentUnavailable,
}

#[derive(Debug, Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
enum IntentAutomationRecordV1 {
    Result {
        protocol_version: u16,
        session_id: String,
        output: Box<ApplicationIntentStackCommandOutputV1>,
    },
    Error {
        protocol_version: u16,
        session_id: String,
        error: IntentAutomationErrorV1,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct IntentAutomationErrorV1 {
    code: IntentAutomationErrorCode,
    message: &'static str,
    retryable: bool,
}

pub(crate) struct IntentCommandExecution {
    pub(crate) exit_code: MachineExitCode,
    record: IntentAutomationRecordV1,
}

impl IntentCommandExecution {
    pub(crate) fn bootstrap_error(
        session_id: impl Into<String>,
        code: IntentAutomationErrorCode,
    ) -> Self {
        Self::error(session_id, code)
    }

    pub(crate) fn write_json(self) -> MachineExitCode {
        match serde_json::to_string(&self.record) {
            Ok(record) => println!("{record}"),
            Err(_) => {
                println!(
                    r#"{{"record_type":"error","protocol_version":1,"session_id":"","error":{{"code":"intent_unavailable","message":"Intent Stack automation output is unavailable.","retryable":true}}}}"#
                );
                return MachineExitCode::ExecutionFailed;
            }
        }
        self.exit_code
    }

    fn success(
        session_id: impl Into<String>,
        output: ApplicationIntentStackCommandOutputV1,
    ) -> Self {
        Self {
            exit_code: MachineExitCode::Success,
            record: IntentAutomationRecordV1::Result {
                protocol_version: INTENT_AUTOMATION_PROTOCOL_VERSION,
                session_id: session_id.into(),
                output: Box::new(output),
            },
        }
    }

    fn error(session_id: impl Into<String>, code: IntentAutomationErrorCode) -> Self {
        let exit_code = match code {
            IntentAutomationErrorCode::InvalidInvocation
            | IntentAutomationErrorCode::ConfigurationInvalid
            | IntentAutomationErrorCode::SessionNotFound
            | IntentAutomationErrorCode::SessionAmbiguous => MachineExitCode::InvalidInput,
            IntentAutomationErrorCode::SessionUnavailable
            | IntentAutomationErrorCode::IntentRequestInvalid
            | IntentAutomationErrorCode::IntentStale
            | IntentAutomationErrorCode::PermissionRequired
            | IntentAutomationErrorCode::IntentConflict
            | IntentAutomationErrorCode::IntentUnavailable => MachineExitCode::ExecutionFailed,
        };
        Self {
            exit_code,
            record: IntentAutomationRecordV1::Error {
                protocol_version: INTENT_AUTOMATION_PROTOCOL_VERSION,
                session_id: session_id.into(),
                error: IntentAutomationErrorV1 {
                    code,
                    message: intent_automation_error_message(code),
                    retryable: matches!(
                        code,
                        IntentAutomationErrorCode::SessionUnavailable
                            | IntentAutomationErrorCode::IntentStale
                            | IntentAutomationErrorCode::IntentConflict
                            | IntentAutomationErrorCode::IntentUnavailable
                    ),
                },
            },
        }
    }
}

pub(crate) fn execute_intent_command(
    config_path: &Path,
    launch_cwd: &Path,
    session_id: &str,
    command: IntentCommand,
) -> IntentCommandExecution {
    if !valid_session_id(session_id) {
        return IntentCommandExecution::error(
            session_id,
            IntentAutomationErrorCode::InvalidInvocation,
        );
    }
    let application_command = match application_command(command) {
        Ok(command) => command,
        Err(code) => return IntentCommandExecution::error(session_id, code),
    };
    let binding = match resolve_session_binding(config_path, launch_cwd, session_id) {
        Ok(binding) => binding,
        Err(code) => return IntentCommandExecution::error(session_id, code),
    };
    match execute_durable_application_intent_stack_command(
        config_path,
        launch_cwd,
        &binding.session_log_path,
        &binding.session_id,
        &application_command,
        ApplicationIntentConfirmationSource::Automation,
    ) {
        Ok(output) => IntentCommandExecution::success(session_id, output),
        Err(error) => IntentCommandExecution::error(
            session_id,
            match error.class() {
                ApplicationIntentStackErrorClass::InvalidRequest => {
                    IntentAutomationErrorCode::IntentRequestInvalid
                }
                ApplicationIntentStackErrorClass::Stale => IntentAutomationErrorCode::IntentStale,
                ApplicationIntentStackErrorClass::PermissionRequired => {
                    IntentAutomationErrorCode::PermissionRequired
                }
                ApplicationIntentStackErrorClass::Conflict => {
                    IntentAutomationErrorCode::IntentConflict
                }
                ApplicationIntentStackErrorClass::Unavailable => {
                    IntentAutomationErrorCode::IntentUnavailable
                }
            },
        ),
    }
}

fn application_command(
    command: IntentCommand,
) -> Result<ApplicationIntentStackCommandV1, IntentAutomationErrorCode> {
    match command {
        IntentCommand::Inspect => Ok(ApplicationIntentStackCommandV1::Inspect),
        IntentCommand::DropPreview {
            intent_id,
            intent_version,
        } => Ok(ApplicationIntentStackCommandV1::PreviewDrop {
            intent_ref: IntentVersionRef::new(
                IntentId::new(intent_id)
                    .map_err(|_| IntentAutomationErrorCode::InvalidInvocation)?,
                intent_version,
            )
            .map_err(|_| IntentAutomationErrorCode::InvalidInvocation)?,
        }),
        IntentCommand::Drop {
            operation_id,
            stack_version,
            preview_digest,
        } => Ok(ApplicationIntentStackCommandV1::ExecuteDrop {
            request: IntentDropRequestV1 {
                operation_id: IntentOperationId::new(operation_id)
                    .map_err(|_| IntentAutomationErrorCode::InvalidInvocation)?,
                stack_version: IntentStackVersion::new(stack_version)
                    .map_err(|_| IntentAutomationErrorCode::InvalidInvocation)?,
                preview_digest: IntentDigest::new(preview_digest)
                    .map_err(|_| IntentAutomationErrorCode::InvalidInvocation)?,
            },
        }),
    }
}

fn resolve_session_binding(
    config_path: &Path,
    launch_cwd: &Path,
    requested_session_id: &str,
) -> Result<LocalSessionReopenBinding, IntentAutomationErrorCode> {
    let root_config = RootConfig::load(config_path)
        .map_err(|_| IntentAutomationErrorCode::ConfigurationInvalid)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let lifecycle = LocalSessionLifecycleService::new(
        paths.workspace_id,
        &paths.session_log_dir,
        &paths.session_exports_root,
    );
    let catalog = lifecycle
        .catalog()
        .map_err(|_| IntentAutomationErrorCode::SessionUnavailable)?;
    let matches = catalog
        .entries
        .into_iter()
        .filter(|entry| entry.session_id.as_deref() == Some(requested_session_id))
        .collect::<Vec<_>>();
    let [entry] = matches.as_slice() else {
        return Err(if matches.is_empty() {
            IntentAutomationErrorCode::SessionNotFound
        } else {
            IntentAutomationErrorCode::SessionAmbiguous
        });
    };
    lifecycle
        .resolve_session_for_reopen(&entry.session_ref, requested_session_id)
        .map_err(|_| IntentAutomationErrorCode::SessionUnavailable)
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

const fn intent_automation_error_message(code: IntentAutomationErrorCode) -> &'static str {
    match code {
        IntentAutomationErrorCode::InvalidInvocation => {
            "Intent Stack automation arguments are invalid."
        }
        IntentAutomationErrorCode::ConfigurationInvalid => {
            "Sigil configuration is unavailable or invalid."
        }
        IntentAutomationErrorCode::SessionNotFound => {
            "The exact durable session was not found in this workspace."
        }
        IntentAutomationErrorCode::SessionAmbiguous => {
            "The durable session identity is ambiguous in this workspace."
        }
        IntentAutomationErrorCode::SessionUnavailable => {
            "The durable session is not currently available for automation."
        }
        IntentAutomationErrorCode::IntentRequestInvalid => {
            "The selected Intent operation is invalid."
        }
        IntentAutomationErrorCode::IntentStale => {
            "The Intent Stack changed. Build a fresh exact preview."
        }
        IntentAutomationErrorCode::PermissionRequired => {
            "Current permission mode or workspace trust does not authorize Intent Drop."
        }
        IntentAutomationErrorCode::IntentConflict => {
            "The Intent operation conflicts with current durable state."
        }
        IntentAutomationErrorCode::IntentUnavailable => {
            "Intent Stack automation is temporarily unavailable."
        }
    }
}

#[cfg(test)]
#[path = "tests/intent_cli_tests.rs"]
mod tests;
