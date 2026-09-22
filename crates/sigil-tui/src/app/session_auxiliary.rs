use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Result;
use sigil_kernel::{SessionReadBudget, SessionRecordReadHandle};

use super::{
    AppState, SessionHistoryEntry,
    session_review::{SessionReviewReducer, SessionReviewSnapshot},
};

#[derive(Debug, Default)]
struct ReviewCursor {
    offset: u64,
    sequence: u64,
    session_id: Option<String>,
    source: Option<sigil_kernel::SessionRecordSourceSnapshot>,
    reducer: SessionReviewReducer,
}

#[derive(Debug, Default)]
pub(super) struct SessionAuxiliaryState {
    scope: Option<(PathBuf, String)>,
    epoch: u64,
    reader: Option<SessionRecordReadHandle>,
    cursor: Arc<Mutex<ReviewCursor>>,
    task: Option<AuxiliaryTask>,
    retired: Vec<AuxiliaryTask>,
    requested: bool,
    history_requested: bool,
    closing: bool,
    child_live_revision: u64,
    last_child_request: Option<(super::AgentView, Instant)>,
    pub(super) submitted_user_inputs: BTreeSet<(sigil_kernel::UserInputIdentityV1, String)>,
    pub(super) workspace_snapshot: Option<String>,
    pub(super) review: Option<SessionReviewSnapshot>,
}

#[derive(Debug)]
struct AuxiliaryTask {
    budget: SessionReadBudget,
    receiver: mpsc::Receiver<Result<AuxiliaryResult>>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct AuxiliaryResult {
    epoch: u64,
    revision: u64,
    workspace_snapshot: Option<String>,
    recovery: Option<sigil_kernel::UserInputDecisionCommandV1>,
    review: Option<SessionReviewSnapshot>,
    history: Option<Vec<SessionHistoryEntry>>,
    notices: Vec<String>,
    child: Option<super::agent_flow::ChildTranscriptQuery>,
    child_live_revision: u64,
}

impl Drop for AuxiliaryTask {
    fn drop(&mut self) {
        self.budget.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl SessionAuxiliaryState {
    fn reset_scope(&mut self, path: &std::path::Path, session_id: &str) {
        let scope = (path.to_owned(), session_id.to_owned());
        if self.scope.as_ref() == Some(&scope) {
            return;
        }
        if let Some(task) = self.task.take() {
            task.budget.cancel();
            self.retired.push(task);
        }
        self.scope = Some(scope);
        self.epoch = self.epoch.saturating_add(1);
        self.reader = None;
        self.cursor = Arc::default();
        self.submitted_user_inputs.clear();
        self.workspace_snapshot = None;
        self.review = None;
        self.requested = true;
        self.history_requested = true;
    }
}

impl AppState {
    /// Startup-only catalog load, before the launcher enters raw terminal mode.
    pub(super) fn load_session_history_before_terminal(&mut self) -> Result<()> {
        self.session_browser.history = query_session_history(
            self.sigil_paths.clone(),
            self.managed_history_writer.clone(),
            &SessionReadBudget::default(),
        )?;
        Ok(())
    }

    pub(crate) fn attach_session_query_reader(&mut self, reader: Option<SessionRecordReadHandle>) {
        self.session_auxiliary
            .reset_scope(&self.session_log_path, &self.session_id);
        if let Some(task) = self.session_auxiliary.task.take() {
            task.budget.cancel();
            self.session_auxiliary.retired.push(task);
        }
        self.session_auxiliary.epoch = self.session_auxiliary.epoch.saturating_add(1);
        self.session_auxiliary.reader = reader;
        self.session_auxiliary.cursor = Arc::default();
        self.session_auxiliary.requested = true;
    }

    pub(super) fn request_session_auxiliary_refresh(&mut self) {
        self.session_auxiliary
            .reset_scope(&self.session_log_path, &self.session_id);
        self.session_auxiliary.requested = true;
    }

    pub(super) fn request_session_history_refresh(&mut self) {
        self.request_session_auxiliary_refresh();
        self.session_auxiliary.history_requested = true;
    }

    pub(super) fn mark_child_live_event(&mut self) {
        self.session_auxiliary.child_live_revision =
            self.session_auxiliary.child_live_revision.saturating_add(1);
    }

    pub(super) fn request_child_transcript_refresh(&mut self) {
        let view = self.agent_panel.active_view.clone();
        let now = Instant::now();
        if self
            .session_auxiliary
            .last_child_request
            .as_ref()
            .is_some_and(|(previous, at)| {
                previous == &view && now.saturating_duration_since(*at) < Duration::from_millis(250)
            })
        {
            return;
        }
        self.session_auxiliary.last_child_request = Some((view, now));
        self.request_session_auxiliary_refresh();
    }

    pub(super) fn has_session_auxiliary_work(&self) -> bool {
        !self.session_auxiliary.closing
            && (self.session_auxiliary.requested
                || self.session_auxiliary.task.is_some()
                || !self.session_auxiliary.retired.is_empty())
    }

    pub(crate) fn cancel_session_auxiliary(&mut self) {
        self.session_auxiliary.closing = true;
        self.session_auxiliary.requested = false;
        if let Some(task) = &self.session_auxiliary.task {
            task.budget.cancel();
        }
        for task in &self.session_auxiliary.retired {
            task.budget.cancel();
        }
    }

    /// Retired observations keep their original reader and cancellation budget until the real
    /// thread exits. A rebuilt UI owns their cleanup but cannot consume their old-scope results.
    pub(crate) fn transfer_session_auxiliary_cleanup_to(&mut self, replacement: &mut Self) {
        self.cancel_session_auxiliary();
        if let Some(task) = self.session_auxiliary.task.take() {
            replacement.session_auxiliary.retired.push(task);
        }
        replacement
            .session_auxiliary
            .retired
            .append(&mut self.session_auxiliary.retired);
    }

    pub(crate) fn poll_session_auxiliary_shutdown(
        &mut self,
    ) -> crate::launcher::shutdown::ShutdownPass {
        use crate::launcher::shutdown::{ShutdownPass, poll_owned_thread};
        self.cancel_session_auxiliary();
        if let Some(task) = self.session_auxiliary.task.take() {
            self.session_auxiliary.retired.push(task);
        }
        let mut pass = ShutdownPass::default();
        for task in &mut self.session_auxiliary.retired {
            pass.observe("session-auxiliary", poll_owned_thread(&mut task.handle));
        }
        self.session_auxiliary
            .retired
            .retain(|task| task.handle.is_some());
        pass
    }

    #[cfg(test)]
    pub(crate) fn join_session_auxiliary_until(&mut self, deadline: Instant) -> Result<()> {
        self.cancel_session_auxiliary();
        if let Some(task) = self.session_auxiliary.task.take() {
            self.session_auxiliary.retired.push(task);
        }
        for task in &mut self.session_auxiliary.retired {
            if let Some(handle) = task.handle.as_ref() {
                while !handle.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                anyhow::ensure!(
                    handle.is_finished(),
                    "session observation interrupted; cleanup_complete=false"
                );
                task.handle
                    .take()
                    .expect("finished observation remains owned")
                    .join()
                    .map_err(|_| {
                        anyhow::anyhow!("session observation panicked; cleanup_complete=false")
                    })?;
            }
        }
        self.session_auxiliary.retired.clear();
        Ok(())
    }

    pub(super) fn poll_session_auxiliary(&mut self) -> bool {
        self.session_auxiliary
            .reset_scope(&self.session_log_path, &self.session_id);
        self.session_auxiliary.retired.retain_mut(|task| {
            if task.handle.as_ref().is_none_or(JoinHandle::is_finished) {
                if let Some(handle) = task.handle.take() {
                    let _ = handle.join();
                }
                false
            } else {
                true
            }
        });
        let result =
            self.session_auxiliary
                .task
                .as_ref()
                .and_then(|task| match task.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(anyhow::anyhow!("session auxiliary query disconnected")))
                    }
                });
        let mut changed = false;
        if let Some(result) = result {
            if let Some(task) = self.session_auxiliary.task.take() {
                self.session_auxiliary.retired.push(task);
            }
            match result {
                Ok(result)
                    if result.epoch == self.session_auxiliary.epoch
                        && result.revision == self.session_browser.current_entries_revision =>
                {
                    self.session_auxiliary.workspace_snapshot = result.workspace_snapshot;
                    if let Some(review) = result.review {
                        self.review.latest_checkpoint_restore_sequence =
                            review.latest_checkpoint_restore_sequence;
                        self.review.readiness_sequences_by_scope =
                            review.readiness_sequences_by_scope.clone();
                        self.session_auxiliary.review = Some(review);
                        self.refresh_session_view_cache();
                    }
                    if let Some(child) = result.child {
                        if result.child_live_revision == self.session_auxiliary.child_live_revision
                        {
                            self.apply_child_transcript_query(child);
                        } else {
                            self.session_auxiliary.requested = true;
                        }
                    }
                    if let Some(history) = result.history {
                        self.session_browser.history = history;
                    }
                    if let Some(notice) = result.notices.last() {
                        self.last_notice = Some(notice.clone());
                    }
                    self.session_browser.history_selected = self
                        .session_browser
                        .history_selected
                        .min(self.filtered_session_indices().len().saturating_sub(1));
                    // Auxiliary completion must not erase text entered while the query was slow.
                    let form = self.composer.pending_user_input.clone();
                    let queue = self.composer.pending_user_input_queue.clone();
                    let plan = self.composer.pending_plan_approval.clone();
                    self.restore_durable_attention_surfaces();
                    if let Some(command) = result.recovery {
                        self.restore_durable_attention_surfaces_with_recovery_command(command);
                    }
                    for next in &mut self.composer.pending_user_input_queue {
                        if let Some(previous) = form
                            .iter()
                            .chain(queue.iter())
                            .find(|previous| same_input_owner(next, previous))
                        {
                            preserve_input_draft(next, previous);
                        }
                    }
                    if let Some(previous) = form.as_ref() {
                        if matches!(previous.source, super::UserInputFormSource::Mcp { .. }) {
                            self.composer.pending_user_input = Some(previous.clone());
                        } else if let Some(index) = self
                            .composer
                            .pending_user_input_queue
                            .iter()
                            .position(|next| same_input_owner(next, previous))
                        {
                            self.composer.pending_user_input_queue_index = index;
                            self.composer.pending_user_input =
                                Some(self.composer.pending_user_input_queue[index].clone());
                        }
                    }
                    if let (Some(previous), Some(next)) =
                        (plan, self.composer.pending_plan_approval.as_mut())
                        && previous.plan_id == next.plan_id
                        && previous.plan_hash == next.plan_hash
                    {
                        next.workbench_open = previous.workbench_open;
                        next.workbench_scroll = previous.workbench_scroll;
                        next.workbench_scroll_extent = previous.workbench_scroll_extent;
                        if next.action_allowed(previous.selected_action) {
                            next.selected_action = previous.selected_action;
                        }
                    }
                    changed = true;
                }
                Ok(stale) => {
                    self.session_auxiliary.requested = true;
                    self.session_auxiliary.history_requested |= stale.history.is_some();
                }
                Err(error) => {
                    self.last_notice = Some(format!("session details unavailable: {error}"));
                    changed = true;
                }
            }
        }
        // A cancelled query retains the single observation slot until its thread exits.
        // Rapid session switches must not accumulate blocked background readers.
        if self.session_auxiliary.closing
            || !self.session_auxiliary.requested
            || self.session_auxiliary.task.is_some()
            || !self.session_auxiliary.retired.is_empty()
        {
            return changed;
        }
        let Some(config) = self.config_snapshot.clone() else {
            self.session_auxiliary.requested = false;
            return changed;
        };
        self.session_auxiliary.requested = false;
        let epoch = self.session_auxiliary.epoch;
        let revision = self.session_browser.current_entries_revision;
        // Child recovery needs only its durable route bindings. Do not copy conversation or
        // tool bodies on the UI thread when starting an auxiliary query.
        let recovery_entries = self
            .session_browser
            .current_entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    sigil_kernel::SessionLogEntry::Control(
                        sigil_kernel::ControlEntry::AgentUserInputRoute(_)
                    )
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let path = self.session_log_path.clone();
        let workspace = self.workspace_root.clone();
        let paths = self.sigil_paths.clone();
        let writer = self.managed_history_writer.clone();
        let history_requested = std::mem::take(&mut self.session_auxiliary.history_requested);
        let child_live_revision = self.session_auxiliary.child_live_revision;
        let child_view = self.agent_panel.active_view.clone();
        let child_previous = self
            .agent_panel
            .active_child_transcript
            .as_ref()
            .map(|child| (child.path.clone(), child.file_signature.clone()));
        let reader = self.session_auxiliary.reader.clone();
        let cursor = Arc::clone(&self.session_auxiliary.cursor);
        let budget = SessionReadBudget::default();
        let work_budget = budget.clone();
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::Builder::new().name("sigil-tui-session-details".to_owned()).spawn(move || {
            let result = (|| -> Result<AuxiliaryResult> {
                work_budget.check()?;
                let mut notices = Vec::new();
                let review = reader.and_then(|reader| match read_review(&reader, &cursor, &work_budget) {
                    Ok(review) => Some(review),
                    Err(error) => { notices.push(format!("session review unavailable: {error}")); None }
                });
                work_budget.check()?;
                let child = super::agent_flow::query_child_transcript(child_view, &path, child_previous);
                work_budget.check()?;
                let recovery = match sigil_runtime::application_run::recoverable_agent_user_input_decision_from_child_sessions(&path, &recovery_entries) {
                    Ok(recovery) => recovery,
                    Err(error) => { notices.push(format!("session questions unavailable: {error}")); None }
                };
                work_budget.check()?;
                let workspace_snapshot = match sigil_runtime::plan_handoff_workspace_snapshot_id(&config, &workspace) {
                    Ok(snapshot) => snapshot,
                    Err(error) => { notices.push(format!("workspace details unavailable: {error}")); None }
                };
                work_budget.check()?;
                let history = if history_requested {
                    match query_session_history(paths, writer, &work_budget) {
                        Ok(history) => Some(history),
                        Err(error) => { notices.push(format!("session history unavailable: {error}")); None }
                    }
                } else { None };
                Ok(AuxiliaryResult { epoch, revision, workspace_snapshot, recovery, review, history, child, child_live_revision, notices })
            })();
            let _ = sender.send(result);
        });
        match handle {
            Ok(handle) => {
                self.session_auxiliary.task = Some(AuxiliaryTask {
                    budget,
                    receiver,
                    handle: Some(handle),
                })
            }
            Err(error) => {
                self.last_notice = Some(format!("session details could not start: {error}"));
                changed = true;
            }
        }
        changed
    }
}

fn same_input_owner(
    left: &super::PendingUserInputForm,
    right: &super::PendingUserInputForm,
) -> bool {
    left.request == right.request
        && left.recovery_command == right.recovery_command
        && left.source == right.source
}

fn preserve_input_draft(
    next: &mut super::PendingUserInputForm,
    previous: &super::PendingUserInputForm,
) {
    next.open = previous.open;
    next.focused_question = previous.focused_question;
    next.focus_actions = previous.focus_actions;
    next.selected_action = previous.selected_action;
    next.drafts = previous.drafts.clone();
    next.plan_revision_editor = previous.plan_revision_editor.clone();
    next.scroll = previous.scroll;
    next.scroll_extent = previous.scroll_extent.clone();
}

fn read_review(
    reader: &SessionRecordReadHandle,
    cursor: &Mutex<ReviewCursor>,
    budget: &SessionReadBudget,
) -> Result<SessionReviewSnapshot> {
    let mut cursor = cursor
        .lock()
        .map_err(|_| anyhow::anyhow!("review cursor lock poisoned"))?;
    let source = reader.source_snapshot(budget)?;
    anyhow::ensure!(
        cursor
            .source
            .as_ref()
            .is_none_or(|previous| previous.same_source(&source))
            && source.byte_len() >= cursor.offset,
        "session review source changed; reattach required"
    );
    let target = source.byte_len();
    while cursor.offset < target {
        budget.check()?;
        let range = reader.read_event_record_range_with_budget(
            cursor.offset,
            cursor.sequence,
            cursor.session_id.as_deref(),
            256,
            (target - cursor.offset).min(4 * 1024 * 1024) as usize,
            budget,
        )?;
        anyhow::ensure!(
            range.source_snapshot().same_source(&source)
                && range.end_offset() > cursor.offset
                && range.end_offset() <= target,
            "session review source changed during query"
        );
        if let Some(record) = range.records().last() {
            cursor.sequence = record.stored_event().stream_sequence;
            cursor.session_id = Some(record.stored_event().session_id.clone());
        }
        cursor.reducer.apply(range.records());
        cursor.offset = range.end_offset();
    }
    cursor.source = Some(source);
    Ok(cursor.reducer.snapshot())
}

fn query_session_history(
    paths: sigil_runtime::paths::SigilPaths,
    writer: Option<Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>>,
    work_budget: &SessionReadBudget,
) -> Result<Vec<SessionHistoryEntry>> {
    let mut lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        paths.session_log_dir.clone(),
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal);
    if let Some(writer) = &writer {
        let root = writer.managed_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
        )?;
        lifecycle = lifecycle
            .with_managed_writer(Arc::clone(writer), paths.workspace_id)?
            .with_managed_session_log_root(root)?;
    }
    let catalog =
        sigil_runtime::SessionCatalogProjectionService::new(lifecycle, paths.session_catalog_db);
    catalog.reconcile()?;
    work_budget.check()?;
    let history = catalog
        .list_workspace_entries()?
        .into_iter()
        .filter(|row| {
            row.source_state == sigil_runtime::LocalSessionCatalogState::Ready
                && row.user_message_count + row.assistant_message_count > 0
        })
        .map(|row| {
            work_budget.check()?;
            let session_ref = sigil_kernel::SessionRef::new_relative(&row.session_ref)?;
            let path = catalog.session_source_path(&session_ref)?;
            Ok(SessionHistoryEntry {
                path,
                label: row.session_ref,
                session_id: row.session_id,
                title: row.title,
                modified_epoch_secs: row.source_modified_at_unix_ms / 1000,
                bytes: row.source_bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(history)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/session_auxiliary_tests.rs"]
mod tests;
