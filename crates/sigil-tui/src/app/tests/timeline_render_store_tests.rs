use std::{ops::Range, time::Instant};

use ratatui::text::Line;

use super::super::{
    TimelineEntry, TimelineRole,
    timeline_render_store::{
        AppendOutcome, RerenderOutcome, TimelineRenderSnapshot, TimelineRenderStore,
    },
};

fn render_options() -> crate::ui::TimelineRenderOptions {
    crate::ui::TimelineRenderOptions {
        max_content_width: 80,
        ..crate::ui::TimelineRenderOptions::default()
    }
}

fn entry(role: TimelineRole, text: &str) -> TimelineEntry {
    TimelineEntry {
        role,
        text: text.to_owned(),
    }
}

fn plain_lines(snapshot: TimelineRenderSnapshot<'_>) -> Vec<String> {
    snapshot.plain_lines_range(0..snapshot.total_lines())
}

fn rendered_lines(snapshot: TimelineRenderSnapshot<'_>) -> Vec<Line<'static>> {
    snapshot.lines_range(0..snapshot.total_lines())
}

fn assert_matches_full_rebuild(
    store: &TimelineRenderStore,
    timeline: &[TimelineEntry],
    options: &crate::ui::TimelineRenderOptions,
) {
    let mut rebuilt = TimelineRenderStore::default();
    rebuilt.rebuild(timeline, options);
    let snapshot = store.snapshot();
    let rebuilt_snapshot = rebuilt.snapshot();

    assert_eq!(snapshot.total_lines(), rebuilt_snapshot.total_lines());
    assert_eq!(plain_lines(snapshot), plain_lines(rebuilt_snapshot));
    assert_eq!(rendered_lines(snapshot), rendered_lines(rebuilt_snapshot));
    assert_eq!(snapshot.prefix_hashes(), rebuilt_snapshot.prefix_hashes());
    for entry_index in 0..timeline.len() {
        assert_eq!(
            snapshot.range_for_entry(entry_index),
            rebuilt_snapshot.range_for_entry(entry_index),
            "entry {entry_index} range should match full rebuild"
        );
    }
    store
        .validate_invariants()
        .expect("incremental store invariants should hold");
}

#[test]
fn timeline_render_store_model_matches_full_rebuild_after_append_and_rerender() {
    let options = render_options();
    let mut timeline = vec![
        entry(TimelineRole::User, "hello"),
        entry(TimelineRole::Assistant, "first answer"),
    ];
    let mut incremental = TimelineRenderStore::default();
    incremental.rebuild(&timeline, &options);

    timeline.push(entry(TimelineRole::Notice, "notice text"));
    incremental.append_entry(&timeline, 2, &options);
    timeline[1].text = "first answer\n\nwith more detail".to_owned();
    incremental.rerender_entry(&timeline, 1, &options);

    let mut rebuilt = TimelineRenderStore::default();
    rebuilt.rebuild(&timeline, &options);

    assert_eq!(
        plain_lines(incremental.snapshot()),
        plain_lines(rebuilt.snapshot())
    );
    assert_eq!(
        incremental.snapshot().prefix_hashes(),
        rebuilt.snapshot().prefix_hashes()
    );
    incremental
        .validate_invariants()
        .expect("incremental store invariants should hold");
}

#[test]
fn timeline_render_store_sequence_matches_full_rebuild_after_mixed_operations() {
    let narrow = crate::ui::TimelineRenderOptions {
        max_content_width: 28,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let wide = crate::ui::TimelineRenderOptions {
        max_content_width: 96,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let mut timeline = vec![
        entry(TimelineRole::User, "hello from a styled user bubble"),
        entry(TimelineRole::Assistant, "assistant answer"),
        entry(TimelineRole::Notice, "notice"),
    ];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &narrow);
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    timeline.push(entry(TimelineRole::Assistant, "appended answer"));
    assert_eq!(
        store.append_entry(&timeline, 3, &narrow),
        AppendOutcome::Appended
    );
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    timeline[1].text = "assistant answer".to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, 1, &narrow),
        RerenderOutcome::Rebuilt
    );
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    timeline[1].text = "short".to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, 1, &narrow),
        RerenderOutcome::Rebuilt
    );
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    timeline[1].text =
        "a much longer assistant answer that should wrap across several lines at narrow width"
            .to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, 1, &narrow),
        RerenderOutcome::Rebuilt
    );
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    timeline.push(entry(
        TimelineRole::User,
        "tail user bubble keeps its styled padding",
    ));
    assert_eq!(
        store.append_entry(&timeline, 4, &narrow),
        AppendOutcome::Appended
    );
    assert_matches_full_rebuild(&store, &timeline, &narrow);

    assert_eq!(
        store.rerender_entry(&timeline, 0, &wide),
        RerenderOutcome::Rebuilt
    );
    assert_matches_full_rebuild(&store, &timeline, &wide);

    timeline.push(entry(TimelineRole::Notice, "after resize append"));
    assert_eq!(
        store.append_entry(&timeline, 5, &wide),
        AppendOutcome::Appended
    );
    assert_matches_full_rebuild(&store, &timeline, &wide);

    let snapshot = store.snapshot();
    let total = snapshot.total_lines();
    let start = total.saturating_sub(3);
    assert_eq!(
        snapshot.lines_range(start..usize::MAX).len(),
        total.saturating_sub(start)
    );
}

#[test]
fn timeline_render_store_rebuilds_append_when_global_options_change() {
    let narrow = crate::ui::TimelineRenderOptions {
        max_content_width: 24,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let wide = crate::ui::TimelineRenderOptions {
        max_content_width: 100,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let mut timeline = vec![entry(
        TimelineRole::Assistant,
        "a long assistant line that wraps differently",
    )];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &narrow);

    timeline.push(entry(TimelineRole::Notice, "new entry"));
    assert_eq!(
        store.append_entry(&timeline, 1, &wide),
        AppendOutcome::Rebuilt
    );
    assert_matches_full_rebuild(&store, &timeline, &wide);
}

#[test]
fn timeline_render_store_trims_separator_without_stale_range() {
    let options = render_options();
    let timeline = vec![
        entry(TimelineRole::Assistant, "one"),
        entry(TimelineRole::Assistant, "two"),
    ];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);
    let snapshot = store.snapshot();
    let plain = plain_lines(snapshot);

    assert!(!plain.last().is_some_and(|line| line.trim().is_empty()));
    assert!(plain.iter().any(|line| line.is_empty()));
    for entry_index in 0..timeline.len() {
        let range = snapshot
            .range_for_entry(entry_index)
            .expect("timeline entry should have a render range");
        assert!(range.end <= snapshot.total_lines());
    }
}

#[test]
fn timeline_render_store_clamps_stale_ranges() {
    let options = render_options();
    let timeline = vec![entry(TimelineRole::Assistant, "answer")];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);
    let snapshot = store.snapshot();

    assert_eq!(
        snapshot.lines_range(0..usize::MAX).len(),
        snapshot.total_lines()
    );
    assert!(snapshot.lines_range(usize::MAX..usize::MAX).is_empty());
    assert!(
        snapshot
            .plain_lines_range(usize::MAX..usize::MAX)
            .is_empty()
    );
}

#[test]
fn timeline_render_store_handles_pending_tool_execution_without_result() {
    let options = render_options();
    let timeline = vec![entry(
        TimelineRole::Tool,
        r#"{"tool_name":"bash","status":"running","preview_kind":"text","preview_lines":["cargo check"],"metadata":{"details":{"call":{"summary":"command=cargo check"}}}}"#,
    )];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);

    let plain = plain_lines(store.snapshot()).join("\n");
    assert!(plain.contains("bash") || plain.contains("cargo check"));
    store
        .validate_invariants()
        .expect("pending tool render store invariants should hold");
}

#[test]
fn timeline_render_store_width_change_rebuilds_prefixes() {
    let narrow = crate::ui::TimelineRenderOptions {
        max_content_width: 24,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let wide = crate::ui::TimelineRenderOptions {
        max_content_width: 100,
        ..crate::ui::TimelineRenderOptions::default()
    };
    let timeline = vec![entry(
        TimelineRole::Assistant,
        "a long assistant line that should wrap differently when the width changes",
    )];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &narrow);
    let narrow_total = store.snapshot().total_lines();
    let narrow_hashes = store.snapshot().prefix_hashes().to_vec();

    store.rebuild(&timeline, &wide);

    assert_ne!(store.snapshot().total_lines(), 0);
    assert_ne!(
        (narrow_total, narrow_hashes),
        (
            store.snapshot().total_lines(),
            store.snapshot().prefix_hashes().to_vec()
        )
    );
}

#[test]
fn timeline_render_store_entry_at_line_matches_ranges() {
    let options = render_options();
    let timeline = vec![
        entry(TimelineRole::User, "hello"),
        entry(TimelineRole::Assistant, "answer"),
        entry(TimelineRole::Notice, "notice"),
    ];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);
    let snapshot = store.snapshot();

    for entry_index in 0..timeline.len() {
        let Range { start, end } = snapshot
            .range_for_entry(entry_index)
            .expect("timeline entry should have a render range");
        for line_index in start..end {
            assert_eq!(snapshot.entry_at_line(line_index), Some(entry_index));
        }
    }
}

#[test]
fn timeline_render_store_fast_paths_match_rebuild_across_hidden_tail_transitions() {
    let options = render_options();
    let mut timeline = (0..128)
        .map(|index| entry(TimelineRole::Notice, &format!("notice {index}")))
        .collect::<Vec<_>>();
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);

    timeline.push(entry(TimelineRole::Assistant, ""));
    assert_eq!(
        store.append_entry(&timeline, timeline.len() - 1, &options),
        AppendOutcome::Appended
    );
    assert_matches_full_rebuild(&store, &timeline, &options);
    assert_eq!(
        store.snapshot().range_for_entry(timeline.len() - 1),
        Some(store.snapshot().total_lines()..store.snapshot().total_lines())
    );

    timeline.push(entry(TimelineRole::Assistant, "streaming"));
    assert_eq!(
        store.append_entry(&timeline, timeline.len() - 1, &options),
        AppendOutcome::Appended
    );
    assert_matches_full_rebuild(&store, &timeline, &options);

    let tail_index = timeline.len() - 1;
    timeline[tail_index].text = "streaming update\nwith more content".to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, tail_index, &options),
        RerenderOutcome::Rerendered
    );
    assert_matches_full_rebuild(&store, &timeline, &options);

    timeline[tail_index].text.clear();
    assert_eq!(
        store.rerender_entry(&timeline, tail_index, &options),
        RerenderOutcome::Rerendered
    );
    assert_matches_full_rebuild(&store, &timeline, &options);

    timeline[tail_index].text = "visible again".to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, tail_index, &options),
        RerenderOutcome::Rerendered
    );
    assert_matches_full_rebuild(&store, &timeline, &options);
}

#[test]
#[ignore = "release-profile long-session performance evidence"]
fn timeline_render_store_long_session_evidence() {
    const ENTRY_COUNT: usize = 5_000;
    const TAIL_RERENDER_COUNT: usize = 250;

    let options = render_options();
    let source = (0..ENTRY_COUNT)
        .map(|index| {
            let role = match index % 3 {
                0 => TimelineRole::User,
                1 => TimelineRole::Assistant,
                _ => TimelineRole::Notice,
            };
            entry(role, &format!("long timeline entry {index}"))
        })
        .collect::<Vec<_>>();

    let rebuild_started = Instant::now();
    let mut rebuilt = TimelineRenderStore::default();
    rebuilt.rebuild(&source, &options);
    let full_rebuild_ms = rebuild_started.elapsed().as_millis();

    let append_started = Instant::now();
    let mut timeline = Vec::with_capacity(ENTRY_COUNT);
    let mut incremental = TimelineRenderStore::default();
    incremental.rebuild(&timeline, &options);
    for item in &source {
        timeline.push(item.clone());
        assert_eq!(
            incremental.append_entry(&timeline, timeline.len() - 1, &options),
            AppendOutcome::Appended
        );
    }
    let sequential_append_ms = append_started.elapsed().as_millis();
    assert_matches_full_rebuild(&incremental, &timeline, &options);

    let rerender_started = Instant::now();
    for revision in 0..TAIL_RERENDER_COUNT {
        timeline
            .last_mut()
            .expect("long timeline has a tail entry")
            .text = format!("streaming tail revision {revision}");
        assert_eq!(
            incremental.rerender_entry(&timeline, timeline.len() - 1, &options),
            RerenderOutcome::Rerendered
        );
    }
    let tail_rerender_ms = rerender_started.elapsed().as_millis();
    assert_matches_full_rebuild(&incremental, &timeline, &options);
    let snapshot = incremental.snapshot();

    println!(
        "SIGIL_LONG_SESSION_EVIDENCE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": "timeline_render_5k",
            "scale": ENTRY_COUNT,
            "elapsed_ms": full_rebuild_ms
                .saturating_add(sequential_append_ms)
                .saturating_add(tail_rerender_ms),
            "facts": {
                "full_rebuild_ms": full_rebuild_ms,
                "sequential_append_ms": sequential_append_ms,
                "tail_rerender_count": TAIL_RERENDER_COUNT,
                "tail_rerender_ms": tail_rerender_ms,
                "total_lines": snapshot.total_lines(),
                "prefix_hash_count": snapshot.prefix_hashes().len(),
            }
        })
    );
}
