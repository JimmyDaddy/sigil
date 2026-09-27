use super::*;

#[test]
fn renderer_diagnostics_cannot_smuggle_payload_and_are_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let valid = RendererRunTiming {
        run_key: "a".repeat(64),
        submission_key: "b".repeat(64),
        phase: RendererTimingPhase::FirstFeedbackFrame,
        elapsed_us: 5_000,
    };
    let mut observations = vec![valid.clone(); 200];
    observations[0].run_key = "private path /Users/example".to_owned();
    let output = supplement_support_bundle("{\"schema_version\":1}", &observations)
        .ok_or("missing output")?;
    let json: serde_json::Value = serde_json::from_str(&output)?;
    assert_eq!(json["schema_version"], 1);
    assert_eq!(
        json["renderer_run_timings"]["observations"]
            .as_array()
            .ok_or("missing observations")?
            .len(),
        191
    );
    assert_eq!(json["renderer_run_timings"]["omitted"], 9);
    assert!(!output.contains("/Users"));
    assert!(supplement_support_bundle("[]", &[valid]).is_none());
    Ok(())
}
