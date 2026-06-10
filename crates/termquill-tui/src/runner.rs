mod approval_bridge;
mod diagnostics;
mod event_bridge;
mod protocol;
mod session_flow;
mod spawn;
mod worker_loop;

pub use protocol::{CompactionTrigger, WorkerCommand, WorkerMessage};
pub use spawn::spawn_agent_worker;

#[cfg(test)]
mod tests;
