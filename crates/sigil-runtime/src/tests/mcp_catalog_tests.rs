use std::{fs, path::Path, sync::Arc};

use anyhow::{Result, anyhow, ensure};
use sigil_kernel::{
    CompletionRequest, FrozenProviderRequestMaterial, McpServerTransportConfig, ModelMessage,
    ToolCall, ToolLifecycleOwner, ToolRegistryScope, ToolResultStatus,
};

use super::*;

fn config() -> Result<RootConfig> {
    RootConfig::parse_persisted(
        "config_version = 2\n[agent]\nconnection = \"fixture\"\nmodel = \"fixture\"\n[composition]\nprofile = \"core\"\nenhancements = [\"mcp\"]\n",
    )
}

fn server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_owned(),
        description: format!("Inspect {name} records"),
        transport: McpServerTransportConfig::Stdio {
            command: "must-not-be-spawned-by-discovery".to_owned(),
            args: vec!["private-command-argument".to_owned()],
            inherit_env: vec!["PRIVATE_ENV_NAME".to_owned()],
        },
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    }
}

struct CatalogFixtureTool {
    name: String,
    schema: Value,
    owner: ToolLifecycleOwner,
}

#[async_trait]
impl Tool for CatalogFixtureTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "Read records by exact ID".to_owned(),
            input_schema: self.schema.clone(),
            category: ToolCategory::Mcp,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(self.owner.clone())
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            &self.name,
            args.to_string(),
            ToolResultMeta::default(),
        ))
    }
}

fn register_fixture(registry: &mut ToolRegistry, name: &str, generation: &str, schema: Value) {
    registry.register(Arc::new(CatalogFixtureTool {
        name: name.to_owned(),
        schema,
        owner: ToolLifecycleOwner::new(
            sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
            "records",
            generation,
        ),
    }));
}

async fn catalog(registry: &ToolRegistry, root: &Path, args: Value) -> Result<ToolResult> {
    registry
        .execute(
            ToolContext::new(root, 5),
            ToolCall {
                id: "catalog-fixture".to_owned(),
                name: TOOL_NAME.to_owned(),
                args_json: args.to_string(),
            },
        )
        .await
}

#[tokio::test]
async fn mcp_catalog_configuration_is_bounded_and_does_not_launch_or_disclose_transport()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = config()?;
    config.mcp_servers = vec![server("records"), server("other")];
    config.mcp_servers[0].description = format!("token=abcdef0123456789 {}", "记录".repeat(500));
    let mut registry = ToolRegistry::new();
    register_mcp_catalog(&mut registry, &config);
    let result = catalog(
        &registry,
        fixture.path(),
        json!({"operation":"list_servers","limit":1}),
    )
    .await?;
    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.returned_entries, Some(1));
    assert!(result.metadata.truncated);
    let first: Value = serde_json::from_str(&result.content)?;
    assert_eq!(first["servers"][0]["server_name"], "other");
    assert_eq!(first["next_cursor"], 1);
    let second = catalog(
        &registry,
        fixture.path(),
        json!({"operation":"list_servers","cursor":1}),
    )
    .await?;
    let content: Value = serde_json::from_str(&second.content)?;
    assert_eq!(content["servers"][0]["description_truncated"], true);
    assert!(!second.content.contains("abcdef0123456789"));
    for hidden in [
        "private-command-argument",
        "PRIVATE_ENV_NAME",
        "must-not-be-spawned",
    ] {
        assert!(!second.content.contains(hidden));
    }
    assert_eq!(fs::read_dir(fixture.path())?.count(), 0);
    assert_eq!(registry.specs().len(), 1);
    let invalid = catalog(
        &registry,
        fixture.path(),
        json!({"operation":"list_servers","limit":0}),
    )
    .await?;
    assert!(matches!(invalid.status, ToolResultStatus::Error(_)));
    Ok(())
}

#[tokio::test]
async fn mcp_catalog_details_use_invoking_scope_and_reject_stale_or_guessed_revisions() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let mut config = config()?;
    config.mcp_servers.push(server("records"));
    let mut registry = ToolRegistry::new();
    register_mcp_catalog(&mut registry, &config);
    let schema = json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]});
    register_fixture(&mut registry, "mcp__records__read", "one", schema.clone());
    register_fixture(&mut registry, "mcp__records__hidden", "one", schema.clone());
    let scoped = registry
        .scoped_with_denies(
            ToolRegistryScope {
                allow_all: true,
                ..ToolRegistryScope::default()
            },
            ToolRegistryScope::from_names_and_prefixes(
                ["mcp__records__hidden"],
                std::iter::empty::<&str>(),
            ),
        )
        .into_registry();
    let list = catalog(&scoped, fixture.path(), json!({"operation":"list_tools"})).await?;
    let list: Value = serde_json::from_str(&list.content)?;
    assert_eq!(list["total"], 1);
    let revision = list["tools"][0]["revision"]
        .as_str()
        .ok_or_else(|| anyhow!("missing revision"))?;
    let request =
        json!({"operation":"describe_tool","tool_name":"mcp__records__read","revision":revision});
    let detail = catalog(&scoped, fixture.path(), request.clone()).await?;
    let detail: Value = serde_json::from_str(&detail.content)?;
    assert_eq!(detail["tool"]["input_schema"], schema);
    let hidden = catalog(
        &scoped,
        fixture.path(),
        json!({"operation":"describe_tool","tool_name":"mcp__records__hidden","revision":revision}),
    )
    .await?;
    assert!(matches!(hidden.status, ToolResultStatus::Error(_)));
    let hidden_call = ToolCall {
        id: "guess".to_owned(),
        name: "mcp__records__hidden".to_owned(),
        args_json: "{}".to_owned(),
    };
    assert!(
        scoped
            .execute(ToolContext::new(fixture.path(), 5), hidden_call)
            .await
            .is_err()
    );
    // Even replacing the same name/spec/lifecycle owner creates a new exact registration.
    register_fixture(&mut registry, "mcp__records__read", "one", schema);
    assert!(matches!(
        catalog(&scoped, fixture.path(), request.clone())
            .await?
            .status,
        ToolResultStatus::Error(_)
    ));
    registry.retire_by_lifecycle_owner(&ToolLifecycleOwner::new(
        sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
        "records",
        "one",
    ));
    assert!(matches!(
        catalog(&scoped, fixture.path(), request).await?.status,
        ToolResultStatus::Error(_)
    ));
    let unbound = McpCatalogTool {
        servers: config.mcp_servers,
    };
    assert!(
        unbound
            .execute(
                ToolContext::new(fixture.path(), 5),
                "unbound".to_owned(),
                json!({"operation":"list_tools"})
            )
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn mcp_catalog_never_truncates_a_schema_into_a_different_contract() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = config()?;
    config.mcp_servers.push(server("records"));
    let mut registry = ToolRegistry::new();
    register_mcp_catalog(&mut registry, &config);
    let schema =
        json!({"type":"object","properties":{"value":{"const":"x".repeat(MAX_OUTPUT_BYTES)}}});
    register_fixture(&mut registry, "mcp__records__large", "one", schema.clone());
    let list = catalog(&registry, fixture.path(), json!({"operation":"list_tools"})).await?;
    let list: Value = serde_json::from_str(&list.content)?;
    let result = catalog(&registry, fixture.path(), json!({"operation":"describe_tool","tool_name":"mcp__records__large","revision":list["tools"][0]["revision"]})).await?;
    assert!(
        matches!(result.status, ToolResultStatus::Error(ref error) if error.kind == ToolErrorKind::ResourceLimit)
    );
    assert_eq!(
        registry
            .spec_for("mcp__records__large")
            .expect("direct contract remains available")
            .input_schema,
        schema
    );
    Ok(())
}

fn measured_input_tokens(
    counter: Option<&sigil_provider_deepseek::DeepSeekV4FlashTokenCounter>,
    tools: Vec<ToolSpec>,
    messages: Vec<ModelMessage>,
) -> Result<Option<u64>> {
    let Some(counter) = counter else {
        return Ok(None);
    };
    let request = CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages,
        tools,
        temperature: None,
        max_tokens: Some(64),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    counter
        .count_frozen_target_input(&FrozenProviderRequestMaterial::freeze(
            "b4-structural-ablation",
            request,
        )?)
        .map(Some)
}

#[tokio::test]
async fn mcp_catalog_real_stdio_structural_ablation() -> Result<()> {
    for tool_count in [4, 120, 240] {
        let fixture = tempfile::tempdir()?;
        let script = fixture.path().join("catalog_server.py");
        let launch_log = fixture.path().join("launched.txt");
        fs::write(
            &script,
            r#"
import json, pathlib, sys
count = int(sys.argv[1])
pathlib.Path(sys.argv[2]).open('a').write('started\n')
tools = [{'name': 'lookup_%03d' % i, 'description': 'Read the exact record family %03d by ID, with optional projections and filters.' % i,
          'inputSchema': {'type': 'object', 'properties': dict({'id': {'type': 'string'}}, **{
              'field_%02d' % n: {'type': 'string', 'description': 'Optional record projection %02d; omitted fields retain the server default.' % n}
              for n in range(16)}), 'required': ['id']}}
         for i in range(count)]
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'serverInfo': {'name': 'catalog-fixture', 'version': '1'}, 'capabilities': {'tools': {}}}
    elif method == 'tools/list': result = {'tools': tools}
    elif method == 'tools/call': result = {'content': [{'type': 'text', 'text': msg['params']['arguments']['id']}]}
    elif 'id' not in msg: continue
    else: result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': msg['id'], 'result': result}), flush=True)
"#,
        )?;
        let mut config = config()?;
        config.storage.state_root =
            sigil_kernel::StorageRoot::Path(fixture.path().join("state").display().to_string());
        config.storage.cache_root =
            sigil_kernel::StorageRoot::Path(fixture.path().join("cache").display().to_string());
        let mut selected = server("records");
        selected.transport = McpServerTransportConfig::Stdio {
            command: "python3".to_owned(),
            args: vec![
                script.display().to_string(),
                tool_count.to_string(),
                launch_log.display().to_string(),
            ],
            inherit_env: Vec::new(),
        };
        config.mcp_servers = vec![selected, server("unselected")];
        let capabilities = crate::provider_capabilities_for_name("deepseek")
            .ok_or_else(|| anyhow!("missing fixture capabilities"))?;
        let mut registry =
            crate::build_tool_registry(&config, &capabilities, fixture.path().to_path_buf())
                .await?;
        let before = catalog(
            &registry,
            fixture.path(),
            json!({"operation":"list_servers"}),
        )
        .await?;
        assert!(matches!(before.status, ToolResultStatus::Ok));
        assert!(!launch_log.exists());
        crate::activate_lazy_mcp_tools_detailed(
            &mut registry,
            &config,
            &capabilities,
            fixture.path().to_path_buf(),
            Some("records"),
        )
        .await?;
        // Retire actual subprocesses before propagating any structural measurement failure.
        let measured = measure_catalog_ablation(&registry, fixture.path(), tool_count).await;
        let retired_tools = registry.drain_by_name_prefix("mcp__records__");
        let cleanup = crate::shutdown_registered_tools(&retired_tools).await;
        cleanup?;
        let measurement = measured?;
        assert_eq!(fs::read_to_string(&launch_log)?.lines().count(), 1);
        println!("B4_ABLATION {}", serde_json::to_string(&measurement)?);
    }
    Ok(())
}

async fn measure_catalog_ablation(
    registry: &ToolRegistry,
    root: &Path,
    tool_count: usize,
) -> Result<Value> {
    // Explicit measurement artifact, never an implicit lookup in the user's cache/config.
    let counter = std::env::var_os("SIGIL_B4_TOKENIZER_PATH")
        .map(|path| {
            sigil_provider_deepseek::DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(
                Path::new(&path),
            )
        })
        .transpose()?;
    let baseline = registry
        .specs()
        .into_iter()
        .filter(|spec| spec.name.starts_with("mcp__records__"))
        .collect::<Vec<_>>();
    ensure!(
        baseline.len() == tool_count,
        "MCP fixture did not publish the requested tool count"
    );
    let catalog_spec = registry
        .spec_for(TOOL_NAME)
        .ok_or_else(|| anyhow!("catalog not registered"))?;
    let initial_messages = vec![
        ModelMessage::system("Use tools for exact record lookup."),
        ModelMessage::user(
            "Read record ID alpha from family 000 and ID beta from the final family.",
        ),
    ];
    let baseline_tokens =
        measured_input_tokens(counter.as_ref(), baseline.clone(), initial_messages.clone())?;
    let mut candidate_messages = initial_messages;
    let mut cursor = 0;
    let mut summaries = Vec::new();
    let mut discovery_calls = 0;
    let mut discovery_input_tokens = Some(0u64);
    loop {
        let args = json!({"operation":"list_tools","server_name":"records","cursor":cursor,"limit":MAX_PAGE_ENTRIES});
        discovery_input_tokens = discovery_input_tokens
            .zip(measured_input_tokens(
                counter.as_ref(),
                vec![catalog_spec.clone()],
                candidate_messages.clone(),
            )?)
            .map(|(total, next)| total + next);
        let result = catalog(registry, root, args.clone()).await?;
        let page: Value = serde_json::from_str(&result.content)?;
        let entries = page["tools"]
            .as_array()
            .ok_or_else(|| anyhow!("catalog page missing tools"))?;
        summaries.extend(entries.iter().cloned());
        let call_id = format!("list-{discovery_calls}");
        candidate_messages.push(ModelMessage::assistant(
            None,
            vec![ToolCall {
                id: call_id.clone(),
                name: TOOL_NAME.to_owned(),
                args_json: args.to_string(),
            }],
        ));
        candidate_messages.push(ModelMessage::tool(call_id, result.content));
        discovery_calls += 1;
        if let Some(next) = page["next_cursor"].as_u64() {
            cursor = next as usize;
        } else {
            break;
        }
    }
    ensure!(
        summaries.len() == tool_count,
        "MCP catalog lost tool summaries"
    );
    let mut selected = Vec::new();
    for index in [0, tool_count - 1] {
        let args = json!({"operation":"describe_tool","tool_name":summaries[index]["tool_name"],"revision":summaries[index]["revision"]});
        discovery_input_tokens = discovery_input_tokens
            .zip(measured_input_tokens(
                counter.as_ref(),
                vec![catalog_spec.clone()],
                candidate_messages.clone(),
            )?)
            .map(|(total, next)| total + next);
        let result = catalog(registry, root, args.clone()).await?;
        let detail: Value = serde_json::from_str(&result.content)?;
        let spec: ToolSpec = serde_json::from_value(detail["tool"].clone())?;
        ensure!(
            serde_json::to_value(&spec)? == serde_json::to_value(&baseline[index])?,
            "MCP catalog returned a different schema"
        );
        selected.push(spec);
        let call_id = format!("describe-{index}");
        candidate_messages.push(ModelMessage::assistant(
            None,
            vec![ToolCall {
                id: call_id.clone(),
                name: TOOL_NAME.to_owned(),
                args_json: args.to_string(),
            }],
        ));
        candidate_messages.push(ModelMessage::tool(call_id, result.content));
        discovery_calls += 1;
    }
    // Production stays eager: catalog reads do not hide, enable, or mutate any actual tool.
    ensure!(
        serde_json::to_value(&baseline)?
            == serde_json::to_value(
                registry
                    .specs()
                    .into_iter()
                    .filter(|spec| spec.name.starts_with("mcp__records__"))
                    .collect::<Vec<_>>()
            )?,
        "MCP catalog mutated the direct tool surface"
    );
    let candidate_schema_bytes = serde_json::to_vec(&selected)?.len();
    selected.push(catalog_spec);
    let candidate_tokens = measured_input_tokens(counter.as_ref(), selected, candidate_messages)?;
    if let Some(output_directory) = std::env::var_os("SIGIL_B4_EXPORT_DIR") {
        let output_directory = Path::new(&output_directory);
        ensure!(
            output_directory.is_absolute(),
            "B4 export directory must be explicit and absolute"
        );
        fs::create_dir_all(output_directory)?;
        let mut details = Vec::new();
        for summary in &summaries {
            let result = catalog(registry, root, json!({"operation":"describe_tool","tool_name":summary["tool_name"],"revision":summary["revision"]})).await?;
            ensure!(!result.is_error(), "B4 detail export failed");
            details.push(serde_json::from_str::<Value>(&result.content)?);
        }
        let snapshot = json!({
            "kind": "sigil-b4-real-stdio-catalog-snapshot-v1",
            "tool_count": tool_count,
            "full_tools": baseline,
            "catalog_spec": registry.spec_for(TOOL_NAME),
            "summaries": summaries,
            "details": details,
            "server_starts": 1,
            "unselected_server_starts": 0,
        });
        fs::write(
            output_directory.join(format!("catalog-{tool_count}.json")),
            serde_json::to_vec_pretty(&snapshot)?,
        )?;
    }
    Ok(json!({
        "tools": tool_count, "baseline_schema_bytes": serde_json::to_vec(&baseline)?.len(),
        "summary_bytes": serde_json::to_vec(&summaries)?.len(), "selected_schema_bytes": candidate_schema_bytes,
        "production_added_catalog_schema_bytes": serde_json::to_vec(&registry.spec_for(TOOL_NAME))?.len(),
        "baseline_input_tokens": baseline_tokens, "candidate_final_input_tokens": candidate_tokens,
        "candidate_all_input_tokens": discovery_input_tokens.zip(candidate_tokens).map(|(total, last)| total + last),
        "extra_discovery_calls": discovery_calls, "typed_exact_schema_matches": 2,
        "model_selection_accuracy": null, "live_cost": null, "server_starts": 1, "unselected_server_starts": 0,
        "production_schema_visibility_unchanged": true,
    }))
}
