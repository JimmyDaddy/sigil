use std::{fs::File, io::Read};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    ToolExecutionCapturePlanV1, ToolOutputStreamLayoutV1, ToolOutputStreamV1, ToolResultRecordedV3,
    ToolSourceCompletenessV1,
};

use super::*;

/// Exports an exact bounded snapshot of the owner's logs, never the display preview.
/// The joined blocking task owns open file handles and the staged artifact sink until settlement.
pub(super) async fn capture_terminal_snapshot(
    ctx: &ToolContext,
    mut result: ToolResult,
    entry: TerminalTaskEntry,
    artifacts: crate::TerminalTaskArtifacts,
) -> Result<ToolResult> {
    let Some(store) = ctx.tool_artifact_store() else {
        return Ok(result);
    };
    let mut plan = ToolExecutionCapturePlanV1::process_defaults(
        store.session_scope_id_hash().to_owned(),
        &result.call_id,
        &result.tool_name,
    );
    if entry.handle.execution_backend.is_some_and(|backend| {
        matches!(
            backend,
            sigil_kernel::TerminalExecutionBackendKind::LocalPty
                | sigil_kernel::TerminalExecutionBackendKind::SandboxedPty
        )
    }) {
        plan.stream_layout = ToolOutputStreamLayoutV1::PtyOrdered;
    }
    let sink = ctx
        .create_policy_safe_tool_output_sink(
            &result.call_id,
            &result.tool_name,
            "text/plain; charset=utf-8",
            ToolArtifactEncoding::Utf8,
            ToolArtifactSensitivity::SensitiveLocal,
        )
        .context("session artifact store disappeared during execution capture")?;
    let capture_store = store.clone();
    let config = plan.process_capture_config();
    let observed_bytes = entry.output_total_bytes;
    let generation = entry.generation;
    let settled = entry.status.is_terminal();
    let captured = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut sink = sink.begin_process_capture(config)?;
        // Opening the handles before reading keeps this snapshot stable across namespace cleanup;
        // exact lengths prevent a live process's later append from entering this capture.
        let sources = [
            (ToolOutputStreamV1::Stdout, File::open(&artifacts.absolute_stdout)?),
            (ToolOutputStreamV1::Stderr, File::open(&artifacts.absolute_stderr)?),
        ].into_iter().map(|(stream, file)| file.metadata().map(|metadata| (stream, file, metadata.len())))
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut source_bytes = 0_u64;
        let mut source_streams = Vec::new();
        for (stream, file, length) in sources {
            let mut reader = file.take(length);
            let mut captured_bytes = 0_u64;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 { break; }
                digest.update(&buffer[..read]);
                captured_bytes = captured_bytes.saturating_add(read as u64);
                sink.write_stream(stream, &buffer[..read])?;
            }
            anyhow::ensure!(captured_bytes == length, "execution output changed during snapshot capture");
            source_bytes = source_bytes.saturating_add(captured_bytes);
            source_streams.push(json!({"stream": stream, "bytes": captured_bytes, "sha256": format!("sha256:{:x}", digest.finalize())}));
        }
        let source = match entry.output_termination_reason {
            Some(sigil_kernel::TerminalOutputTerminationReason::OutputCaptureFailed | sigil_kernel::TerminalOutputTerminationReason::OutputDrainTimeout) => ToolSourceCompletenessV1::ReaderFailed,
            Some(sigil_kernel::TerminalOutputTerminationReason::OutputLimitExceeded) => ToolSourceCompletenessV1::ResourceLimited,
            None if settled && source_bytes < observed_bytes => ToolSourceCompletenessV1::ResourceLimited,
            None if matches!(entry.status, TerminalTaskStatus::Exited { .. }) => ToolSourceCompletenessV1::Complete,
            None => ToolSourceCompletenessV1::Interrupted,
        };
        let (descriptor, segments, completeness) = sink.finish_process_capture(source_bytes.max(observed_bytes), 0, source)?;
        let preview = if entry.output_preview.is_none() && source_bytes > 0 {
            let head = capture_store.read_page(&descriptor.artifact_ref, sigil_kernel::ToolArtifactSelectorV1::ByteSlice { offset: 0, limit: 8 * 1024 })?;
            let content = if head.eof {
                head.body
            } else {
                let tail = capture_store.read_page(&descriptor.artifact_ref, sigil_kernel::ToolArtifactSelectorV1::ByteSlice { offset: descriptor.persisted_bytes.saturating_sub(8 * 1024), limit: 8 * 1024 })?;
                format!("{}\n... output omitted; use exec_read ...\n{}", head.body, tail.body)
            };
            Some(crate::support::limit_text_head_tail(&content, 16 * 1024))
        } else { None };
        Ok((descriptor, segments, completeness, source_streams, preview))
    }).await.context("execution output capture task panicked")?;
    match captured {
        Ok((descriptor, segments, completeness, source_streams, preview)) => {
            result.metadata.total_bytes = Some(descriptor.observed_bytes);
            result.metadata.details["output_total_bytes"] = json!(descriptor.observed_bytes);
            if let Some(preview) = preview {
                result.content.push('\n');
                result.content.push_str(&preview.content);
                result.metadata.returned_lines = Some(preview.returned_lines);
                result.metadata.details["output_preview"] = json!(preview.content);
            }
            result.metadata.details["capture"] = json!({
                "scope": if settled { "settled_execution" } else { "execution_snapshot" },
                "generation": generation, "streams": source_streams,
                "source": completeness.source, "storage": completeness.storage,
            });
            // Non-V3 consumers must reference the same full capture too, never recapture the
            // bounded lifecycle/display content as if it were the command's complete output.
            result = result.with_captured_artifact(descriptor.clone());
            match ToolResultRecordedV3::from_process_capture(
                &result,
                descriptor,
                &plan,
                segments,
                completeness,
                sigil_kernel::tool_model_view_initial_limit(&result.tool_name),
            ) {
                Ok((recorded, display)) => result.set_durable_v3_projection(recorded, display),
                Err(_) => {
                    result = capture_unavailable(
                        result,
                        observed_bytes,
                        settled,
                        "capture_settlement_failed",
                    )
                }
            }
        }
        Err(_) => {
            result = capture_unavailable(result, observed_bytes, settled, "capture_storage_failed")
        }
    }
    Ok(result)
}

fn capture_unavailable(
    mut result: ToolResult,
    observed_bytes: u64,
    settled: bool,
    stage: &str,
) -> ToolResult {
    result.metadata.details["capture"] = json!({
        "code": "capture_storage_failed", "stage": stage, "observed_bytes": observed_bytes,
        "command_completed": settled,
        "action": "use exec_read with execution_id to inspect the saved output; do not rerun the command",
    });
    result.with_unavailable_artifact_capture(observed_bytes)
}
