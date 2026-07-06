use anyhow::Result;
use sigil_kernel::{
    AgentThreadTerminalStatus, ControlEntry, EventHandler, RunEvent, Session,
    TaskChildSessionStatus,
};

pub(super) fn append_control<H>(
    session: &mut Session,
    handler: &mut H,
    control: ControlEntry,
) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

pub(super) fn agent_terminal_status_from_task_child(
    status: TaskChildSessionStatus,
) -> AgentThreadTerminalStatus {
    match status {
        TaskChildSessionStatus::Completed => AgentThreadTerminalStatus::Completed,
        TaskChildSessionStatus::Failed | TaskChildSessionStatus::Unavailable => {
            AgentThreadTerminalStatus::Failed
        }
        TaskChildSessionStatus::Cancelled => AgentThreadTerminalStatus::Cancelled,
        TaskChildSessionStatus::Interrupted | TaskChildSessionStatus::Started => {
            AgentThreadTerminalStatus::Interrupted
        }
    }
}
