//! Shell check evidence tests for the verification runner.

use super::*;

fn shell_check(args: &[&str]) -> CheckCommand {
    CheckCommand {
        command: "bash".to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: None,
    }
}

#[test]
fn shell_check_evidence_distinguishes_final_stage_only_from_observed_pipeline() {
    assert_eq!(
        assess_shell_check(&shell_check(&["-c", "false | tail -1"]), 0),
        ShellCheckAssessment::FinalStageOnly {
            shell_supports_pipefail: true
        }
    );
    assert_eq!(
        assess_shell_check(
            &shell_check(&["-o", "pipefail", "-c", "false | tail -1"]),
            1
        ),
        ShellCheckAssessment::AllStagesObserved
    );
    assert_eq!(
        assess_shell_check(
            &CheckCommand {
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: None,
            },
            0,
        ),
        ShellCheckAssessment::Safe
    );
}

#[test]
fn shell_check_evidence_marks_echo_after_failure_as_unproven() {
    assert_eq!(
        assess_shell_check(&shell_check(&["-c", "false; echo finished"]), 0),
        ShellCheckAssessment::FinalStageOnly {
            shell_supports_pipefail: true
        }
    );
}

#[test]
fn shell_check_evidence_does_not_treat_output_text_as_pipefail_configuration() {
    assert_eq!(
        assess_shell_check(&shell_check(&["-c", "echo pipefail; false | cat"]), 0),
        ShellCheckAssessment::FinalStageOnly {
            shell_supports_pipefail: true
        }
    );
    assert_eq!(
        assess_shell_check(&shell_check(&["-lc", "false | cat"]), 0),
        ShellCheckAssessment::FinalStageOnly {
            shell_supports_pipefail: true
        }
    );
}
