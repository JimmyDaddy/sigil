use super::super::{AppState, RunPhase};

impl AppState {
    pub(super) fn start_worker_run_phase(
        &mut self,
        phase: RunPhase,
        notice: impl Into<String>,
        phase_marker: impl Into<String>,
    ) {
        self.runtime.is_busy = true;
        self.runtime.run_phase = phase;
        self.runtime.mcp_progress = None;
        self.last_notice = Some(notice.into());
        self.push_phase_marker(phase_marker.into());
    }

    pub(super) fn clear_worker_run_state(&mut self) {
        self.runtime.is_busy = false;
        self.runtime.run_phase = RunPhase::Idle;
        self.runtime.mcp_progress = None;
        self.approval.pending = None;
        self.modal_state = None;
        self.runtime.last_phase_marker = None;
        self.clear_recent_egress_disclosure();
    }

    pub(super) fn finish_worker_streams(&mut self) {
        self.finish_streaming_assistant_entry();
        self.finish_streaming_reasoning_entry();
    }

    pub(super) fn discard_worker_streaming_assistant_and_finish_reasoning(&mut self) {
        self.timeline_state.streaming_assistant_index = None;
        self.finish_streaming_reasoning_entry();
    }
}
