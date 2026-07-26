use sigil_eval_orch_pos_cross_layer_04::render_record;

#[test]
fn normalizes_then_wraps_the_public_result() {
    assert_eq!(render_record("  MiXeD-42  "), "[mixed-42]");
}
