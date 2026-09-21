use sigil_eval_agent_collaboration_v1::render_record;

#[test]
fn trims_input_before_applying_the_record_prefix() {
    assert_eq!(render_record("  alpha  "), "record:alpha");
}
