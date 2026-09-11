use super::*;

pub(super) fn dispatch_maintenance_command<P>(
    _context: WorkerCommandContext<'_, P>,
    command: MaintenanceCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    match command {
        // The scheduler owns the single cleanup path, including unexpected inbox disconnects.
        MaintenanceCommand::Shutdown => WorkerCommandDispatchControl::Break,
    }
}
