use anyhow::Result;
use sigil_kernel::{
    ContextItem, PluginHookContextItems, PluginHookContextOptions, PluginHookOutputEnvelope,
    TaskMemoryV1, plugin_hook_output_context_items, task_memory_context_items,
};

/// Converts typed task memory into runtime context candidates with provenance preserved.
///
/// This is intentionally a thin runtime boundary over the kernel adapter: runtime callers can
/// assemble context without depending on TUI-specific rendering, while the trust/sensitivity
/// invariants remain enforced by kernel types.
pub fn context_items_from_task_memory(memory: &TaskMemoryV1) -> Result<Vec<ContextItem>> {
    task_memory_context_items(memory)
}

/// Converts trusted plugin hook output into runtime context candidates with provenance preserved.
///
/// Runtime callers must still decide when to execute hooks and whether to pass the resulting items
/// into prompt assembly. The helper never creates verification evidence or task-memory facts.
pub fn context_items_from_plugin_hook_output(
    output: &PluginHookOutputEnvelope,
    options: PluginHookContextOptions,
) -> Result<PluginHookContextItems> {
    plugin_hook_output_context_items(output, options)
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
