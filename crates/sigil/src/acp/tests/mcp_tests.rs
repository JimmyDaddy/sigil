use super::*;

#[test]
fn client_stdio_environment_stays_per_process_and_names_remain_exact() {
    let declaration = acp::McpServer::Stdio(
        acp::McpServerStdio::new("editor-server", "/fixture/server").env(vec![
            acp::EnvVariable::new("EXAMPLE_TOKEN", "private-fixture-value"),
        ]),
    );
    let result = declarations(vec![declaration]).expect("valid stdio declaration");
    assert_eq!(result[0].server.name, "editor-server");
    assert_eq!(
        result[0].environment["EXAMPLE_TOKEN"].expose_secret(),
        "private-fixture-value"
    );
    assert!(!format!("{:?}", result[0]).contains("private-fixture-value"));
    assert!(result[0].server.stdio().expect("stdio").2.is_empty());
}

#[test]
fn invalid_or_duplicate_client_declarations_are_not_silently_omitted() {
    let declaration = acp::McpServer::Stdio(acp::McpServerStdio::new("same", "/fixture/server"));
    assert!(declarations(vec![declaration.clone(), declaration]).is_err());
    assert!(
        declarations(vec![acp::McpServer::Stdio(acp::McpServerStdio::new(
            "relative", "server"
        ))])
        .is_err()
    );
}
