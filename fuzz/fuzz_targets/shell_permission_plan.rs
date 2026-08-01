#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use serde_json::json;
use sigil_kernel::{ToolCall, ToolContext, ToolRegistry};
use sigil_tools_builtin::register_builtin_tools;

const MAX_FUZZ_COMMAND_BYTES: usize = 16 * 1024;

fn registry() -> &'static ToolRegistry {
    static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = ToolRegistry::new();
        register_builtin_tools(&mut registry);
        registry
    })
}

fuzz_target!(|bytes: &[u8]| {
    if bytes.len() > MAX_FUZZ_COMMAND_BYTES {
        return;
    }
    let Ok(command) = std::str::from_utf8(bytes) else {
        return;
    };
    let Ok(args_json) = serde_json::to_string(&json!({ "command": command })) else {
        return;
    };
    let call = ToolCall {
        id: "fuzz-shell-permission-plan".to_owned(),
        name: "bash".to_owned(),
        args_json,
    };
    let context = ToolContext::new(".", 5);

    // Both a valid plan and a fail-closed error are acceptable for arbitrary input. The target
    // exists to prove that the production parser/planner stays bounded and never panics.
    let _ = registry().permission_plan(&context, &call);
});
