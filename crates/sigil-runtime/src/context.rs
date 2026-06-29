use anyhow::Result;
use sigil_kernel::{ContextItem, TaskMemoryV1, task_memory_context_items};

/// Converts typed task memory into runtime context candidates with provenance preserved.
///
/// This is intentionally a thin runtime boundary over the kernel adapter: runtime callers can
/// assemble context without depending on TUI-specific rendering, while the trust/sensitivity
/// invariants remain enforced by kernel types.
pub fn context_items_from_task_memory(memory: &TaskMemoryV1) -> Result<Vec<ContextItem>> {
    task_memory_context_items(memory)
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
