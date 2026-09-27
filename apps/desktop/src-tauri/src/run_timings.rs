//! A closed, bounded projection of optional renderer timing observations for support export.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RendererTimingPhase {
    InputAccepted,
    FirstFeedbackFrame,
    Admission,
    FirstContent,
    CancellationRequested,
    CancellationSettled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RendererRunTiming {
    run_key: String,
    submission_key: String,
    phase: RendererTimingPhase,
    elapsed_us: u64,
}

/// Invalid optional observations are omitted; server diagnostics remain exportable.
pub(crate) fn supplement_support_bundle(
    content: &str,
    timings: &[RendererRunTiming],
) -> Option<String> {
    let mut bundle: serde_json::Value = serde_json::from_str(content).ok()?;
    let object = bundle.as_object_mut()?;
    let admitted: Vec<_> = timings
        .iter()
        .take(192)
        .filter(|entry| {
            entry.run_key.len() == 64
                && entry.run_key.bytes().all(|byte| byte.is_ascii_hexdigit())
                && entry.submission_key.len() == 64
                && entry
                    .submission_key
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                && entry.elapsed_us <= 7 * 24 * 60 * 60 * 1_000_000
        })
        .collect();
    object.insert(
        "renderer_run_timings".to_owned(),
        serde_json::json!({
            "clock": "renderer_performance",
            "origin": "input_accepted",
            "observations": admitted,
            "omitted": timings.len().saturating_sub(admitted.len()),
        }),
    );
    if let Some(included) = object
        .get_mut("doctor")
        .and_then(|doctor| doctor.get_mut("privacy"))
        .and_then(|privacy| privacy.get_mut("included"))
        .and_then(serde_json::Value::as_array_mut)
    {
        included.push(serde_json::Value::String(
            "bounded_renderer_run_timings".to_owned(),
        ));
    }
    let content = serde_json::to_string_pretty(&bundle).ok()?;
    (content.len() <= 384 * 1024).then_some(content)
}

#[cfg(test)]
#[path = "tests/run_timings_tests.rs"]
mod tests;
