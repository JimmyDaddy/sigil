use super::*;

#[test]
fn process_group_configuration_is_portable() {
    let mut command = Command::new(if cfg!(windows) { "cmd.exe" } else { "sh" });
    configure_mcp_process_group(&mut command);
}
