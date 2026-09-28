//! Ordinary HTTP input settles only from the actual runtime's durable admission.

use super::*;
use base64::Engine as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_prompt_operation_settles_exact_run_and_replays_after_client_reconnect()
-> Result<()> {
    for (image_only, inline_skill) in [(false, false), (true, false), (false, true), (true, true)] {
        let temp = tempfile::tempdir()?;
        let driver = production_queue_driver(&temp, "prompt-operation");
        if image_only {
            let path = temp.path().join("sigil.toml");
            let config = std::fs::read_to_string(&path)?;
            std::fs::write(
                path,
                config
                    .replace("gpt-test", "gpt-4.1")
                    .replace("chat_completions", "responses"),
            )?;
        }
        let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
            temp.path().join("commands.json"),
            16,
        )?))?;
        let session = registry.create_session(HttpSessionCreateRequest::default())?;
        let attachments = if image_only {
            vec![registry.ingest_image(base64::engine::general_purpose::STANDARD.decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==",
            )?)?]
        } else {
            Vec::new()
        };
        let prompt = if image_only {
            ""
        } else {
            "continue the ordinary conversation"
        };
        let digest = sigil_kernel::conversation_run_input_digest(prompt, &attachments)?;
        let mut command = HttpCommandEnvelope::new(
            "prompt-once",
            "prompt-client",
            &session.id,
            HttpRunStartRequest {
                prompt: prompt.to_owned(),
                image_attachments: attachments,
                review_annotations: Vec::new(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        );
        if inline_skill {
            let skill = temp.path().join(".sigil/skills/review-input/SKILL.md");
            std::fs::create_dir_all(skill.parent().context("skill directory")?)?;
            std::fs::write(
                skill,
                "---\nname: review-input\ndescription: Inspect input.\ntrust: trusted\nuser-invocable: true\nrun-as: inline\n---\nInspect the supplied input.\n",
            )?;
            command.payload.skill_binding = registry
                .run_context_view(&session.id)?
                .extension_catalog
                .skills
                .into_iter()
                .find(|skill| skill.id == "review-input")
                .and_then(|skill| skill.binding);
            assert!(
                command.payload.skill_binding.is_some(),
                "fixture must select a real exact skill binding"
            );
        }
        let client = registry.application_client(&session.id, "prompt-client")?;
        client.refresh()?;
        command.command_journal = Some(client.command_journal_binding()?);
        drop(client);
        let submit = |command: HttpCommandEnvelope<HttpRunStartRequest>| {
            let registry = registry.clone();
            let id = session.id.clone();
            tokio::task::spawn_blocking(move || registry.start_run_command(&id, command))
        };
        let first = submit(command.clone()).await??;
        let reader = driver
            .application_operation_owner(&session)?
            .owner
            .read_handle();
        let records = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let records = reader.read_event_records()?;
                if records.iter().any(|record| {
                    matches!(
                        record.session_log_entry(),
                        Ok(Some(SessionLogEntry::Control(
                            ControlEntry::ConversationRunAcceptedV1(_)
                        )))
                    )
                }) && registry.get_run(&first.run.id)?.status.is_terminal()
                {
                    break anyhow::Ok(records);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let accepted = records
            .iter()
            .filter_map(|record| match record.session_log_entry() {
                Ok(Some(SessionLogEntry::Control(ControlEntry::ConversationRunAcceptedV1(
                    entry,
                )))) => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].run_id, first.run.id);
        assert_eq!(accepted[0].input_digest, digest);
        let commit = records
            .iter()
            .find_map(|record| match record.session_log_entry() {
                Ok(Some(SessionLogEntry::Control(
                    ControlEntry::ApplicationOperationCommittedV1(entry),
                ))) => Some(entry),
                _ => None,
            })
            .context("actual operation commit")?;
        assert!(
            sigil_kernel::session::reconcile_application_operation(&reader, &commit.binding)?
                .is_some()
        );
        // A fresh client reconstructs its receipt from durable causal proof. A terminal run or
        // a transport acknowledgement alone cannot provide this accepted outcome.
        let client = registry.application_client(&session.id, "prompt-client")?;
        client.refresh()?;
        let options = crate::application_bridge::application_run_start_options(&command.payload)?;
        let ordinary = if image_only {
            ConversationCommand::SubmitPromptWithAttachments {
                prompt: None,
                attachments: command.payload.image_attachments.clone(),
                options: Some(Box::new(options)),
            }
        } else {
            ConversationCommand::SubmitPrompt {
                prompt: Some(SafeText::new(prompt)?),
                options: Some(Box::new(options)),
            }
        };
        let receipt = client.execute_in_journal(
            "prompt-once",
            command.command_journal.clone(),
            ApplicationCommand::Conversation(ordinary),
        )?;
        let settled = match receipt {
            ApplicationCommandReceipt::Settled(receipt)
            | ApplicationCommandReceipt::Replayed(receipt) => receipt,
            other => panic!("accepted prompt must no longer remain uncertain: {other:?}"),
        };
        assert!(
            matches!(settled.outcome.as_deref(), Some(sigil_application::ApplicationCommandOutcome::ConversationRunAccepted { run_id }) if run_id.as_str() == first.run.id)
        );
        let operation_id = commit.binding.operation_id.clone();
        let attachment_bindings = serde_json::to_value(&command.payload.image_attachments)?;
        let own_evidence = |records: &[sigil_kernel::SessionStreamRecord]| -> Result<Vec<(String, serde_json::Value)>> {
            let mut evidence = Vec::new();
            for record in records {
                let Some(entry) = record.session_log_entry()? else { continue; };
                let matches = match &entry {
                    SessionLogEntry::User(message) if image_only => !message.image_attachments.is_empty()
                        && serde_json::to_value(&message.image_attachments)? == attachment_bindings,
                    SessionLogEntry::User(message) => message.content.as_deref() == Some(prompt),
                    SessionLogEntry::Control(ControlEntry::ConversationRunAcceptedV1(entry)) => entry.run_id == first.run.id,
                    SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(entry)) => entry.operation_id == operation_id,
                    SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(entry)) => entry.binding.operation_id == operation_id,
                    _ => false,
                };
                if matches { evidence.push((record.event_id().to_owned(), serde_json::to_value(entry)?)); }
            }
            Ok(evidence)
        };
        let before = own_evidence(&reader.read_event_records()?)?;
        let replay = submit(command).await??;
        assert!(replay.replayed);
        assert_eq!(replay.run.id, first.run.id);
        assert_eq!(
            own_evidence(&reader.read_event_records()?)?,
            before,
            "retry must not duplicate its own input/admission/operation evidence; unrelated title or outbox appends are allowed"
        );
        let joined_driver = Arc::clone(&driver);
        tokio::task::spawn_blocking(move || joined_driver.wait_for_idle(Duration::from_secs(20)))
            .await??;
    }
    Ok(())
}
