use super::*;

pub(super) fn dispatch_maintenance_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: MaintenanceCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent: _,
        root_config,
        provider_capabilities: _,
        workspace_root: _,
        options: _,
        message_tx,
        elicitation_handler,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        state,
    } = context;
    match command {
        MaintenanceCommand::Shutdown => {
            if let Some(active_run) = state.run.active.take() {
                cancel_active_run(
                    active_run,
                    runtime,
                    root_config,
                    &state.session.log_path,
                    &mut state.session.current,
                    message_tx,
                    elicitation_handler,
                    &state.agent.supervisor,
                    &mut state.run.discarded_ids,
                    "run interrupted by TUI shutdown",
                );
            }
            state.refresh.provider_status_tasks.abort_all();
            state.compaction.preparation_tasks.abort_all();
            WorkerCommandDispatchControl::Break
        }
    }
}
