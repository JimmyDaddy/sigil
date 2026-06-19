fn main() {
    println!("cargo::rerun-if-env-changed=SIGIL_TUI_TEST_SLICE_APP_INPUT_FLOW");
    println!("cargo::rustc-check-cfg=cfg(sigil_tui_test_slice_app_input_flow)");
    if std::env::var_os("SIGIL_TUI_TEST_SLICE_APP_INPUT_FLOW").is_some() {
        println!("cargo::rustc-cfg=sigil_tui_test_slice_app_input_flow");
    }
}
