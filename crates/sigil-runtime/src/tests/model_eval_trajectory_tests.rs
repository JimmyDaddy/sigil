use super::*;
use sigil_kernel::run_diagnostics::RunTimingObservation;

fn observation(
    sequence: u64,
    run: &str,
    phase: RunTimingPhase,
    elapsed_us: u64,
) -> RunTimingObservation {
    RunTimingObservation {
        sequence,
        run_key: run_timing_key(run),
        phase,
        observed_at_ms: sequence * 7,
        elapsed_us,
    }
}

fn snapshot(observations: Vec<RunTimingObservation>) -> RunTimingSnapshot {
    RunTimingSnapshot {
        process_instance: "fixture-process".to_owned(),
        available: true,
        dropped: 0,
        observations,
    }
}

fn durations(timings: &ModelEvalStageTimings, phase: RunTimingPhase) -> Option<&[u64]> {
    timings
        .phases
        .iter()
        .find(|value| value.phase == phase)
        .and_then(|value| value.elapsed_us.as_deref())
}

#[test]
fn model_eval_timings_require_exact_run_and_new_sequence_without_exporting_identity() {
    let run = "run-with-private-looking-input";
    let before = snapshot(vec![observation(10, run, RunTimingPhase::Preparation, 900)]);
    let after = snapshot(vec![
        observation(10, run, RunTimingPhase::Preparation, 900),
        observation(11, "other-run", RunTimingPhase::FirstFeedbackFrame, 12),
        observation(12, "other-run", RunTimingPhase::Preparation, 700),
        observation(13, run, RunTimingPhase::Preparation, 31),
        observation(14, run, RunTimingPhase::ProviderFirstContent, 84),
        observation(15, run, RunTimingPhase::ProviderFirstContent, 29),
    ]);
    let timings = ModelEvalStageTimings::observe(run, &before, &after);
    assert!(timings.snapshots_available);
    assert_eq!(timings.global_dropped_during_turn, Some(0));
    assert_eq!(
        durations(&timings, RunTimingPhase::Preparation),
        Some([31].as_slice())
    );
    assert_eq!(
        durations(&timings, RunTimingPhase::ProviderFirstContent),
        Some([84, 29].as_slice())
    );
    assert_eq!(
        durations(&timings, RunTimingPhase::FirstFeedbackFrame),
        None
    );
    assert_eq!(durations(&timings, RunTimingPhase::ToolExecution), None);
    let wire = serde_json::to_string(&timings).expect("timing side table");
    for private in [run, "other-run", "fixture-process", &run_timing_key(run)] {
        assert!(!wire.contains(private));
    }
    assert!(!wire.contains("observed_at_ms"));
    assert!(!wire.contains("sequence"));
}

#[test]
fn model_eval_unavailable_or_different_process_timings_remain_unknown() {
    let before = snapshot(Vec::new());
    let after = snapshot(vec![observation(1, "run", RunTimingPhase::Preparation, 8)]);
    for (mut before, mut after, missing) in [
        (before.clone(), after.clone(), "before"),
        (before.clone(), after.clone(), "after"),
        (before, after, "process"),
    ] {
        match missing {
            "before" => before.available = false,
            "after" => after.available = false,
            "process" => after.process_instance = "different-process".to_owned(),
            _ => unreachable!(),
        }
        let timings = ModelEvalStageTimings::observe("run", &before, &after);
        assert!(!timings.snapshots_available);
        assert_eq!(timings.global_dropped_during_turn, None);
        assert!(
            timings
                .phases
                .iter()
                .all(|phase| phase.elapsed_us.is_none())
        );
    }
}

#[test]
fn model_eval_eviction_reports_only_global_window_drops_and_retained_exact_samples() {
    let mut before = snapshot(vec![observation(
        100,
        "older-run",
        RunTimingPhase::Preparation,
        1,
    )]);
    before.dropped = 7;
    // Earlier observations for this run may have been evicted. A different run's UI timing
    // cannot fill the gap, and the process-global loss count is not a per-run measurement.
    let mut after = snapshot(vec![
        observation(400, "other-run", RunTimingPhase::FirstFeedbackFrame, 13),
        observation(401, "run", RunTimingPhase::ToolExecution, 77),
    ]);
    after.dropped = 301;
    let timings = ModelEvalStageTimings::observe("run", &before, &after);
    assert_eq!(timings.global_dropped_during_turn, Some(294));
    assert_eq!(
        durations(&timings, RunTimingPhase::ToolExecution),
        Some([77].as_slice())
    );
    assert_eq!(durations(&timings, RunTimingPhase::Preparation), None);
    assert_eq!(
        durations(&timings, RunTimingPhase::FirstFeedbackFrame),
        None
    );
    let summary = serde_json::to_value(ModelEvalTrajectorySummary::default()).expect("summary");
    assert!(summary["redundant_reads"].is_null());
    assert!(summary["human_interventions"].is_null());
    assert!(summary["ineffective_repair_rounds"].is_null());
}
