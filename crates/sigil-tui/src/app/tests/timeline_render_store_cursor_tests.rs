use super::*;

#[test]
fn tail_rerender_reuses_stable_markdown_layout() {
    let options = crate::ui::TimelineRenderOptions {
        max_content_width: 80,
        streaming_assistant_index: Some(0),
        ..crate::ui::TimelineRenderOptions::default()
    };
    let mut timeline = vec![TimelineEntry {
        role: TimelineRole::Assistant,
        text: "Stable paragraph.\n\nLive".to_owned(),
    }];
    let mut store = TimelineRenderStore::default();
    store.rebuild(&timeline, &options);

    timeline[0].text = "Stable paragraph.\n\nLive tail".to_owned();
    assert_eq!(
        store.rerender_entry(&timeline, 0, &options),
        RerenderOutcome::Rerendered
    );
    assert_eq!(store.last_reused_markdown_blocks(), 1);

    let mut rebuilt = TimelineRenderStore::default();
    rebuilt.rebuild(&timeline, &options);
    assert_eq!(
        store
            .snapshot()
            .plain_lines_range(0..store.snapshot().total_lines()),
        rebuilt
            .snapshot()
            .plain_lines_range(0..rebuilt.snapshot().total_lines())
    );
}
