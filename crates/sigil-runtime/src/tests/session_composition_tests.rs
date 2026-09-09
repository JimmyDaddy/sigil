use anyhow::Result;
use sigil_kernel::{ModelMessage, OptionalCapability, RuntimeCompositionConfig};

use super::*;

fn core_config() -> RootConfig {
    let mut config = crate::provider_connections::default_setup_root_config();
    config.composition = RuntimeCompositionConfig::core();
    config
}

#[test]
fn composition_binding_survives_repeated_preparation() -> Result<()> {
    let mut session = Session::new("fixture", "model");
    let config = core_config();
    session.append_user_message(ModelMessage::user("queued input"))?;
    bind_session_composition(&mut session, &config)?;
    bind_session_composition(&mut session, &config)?;
    assert_eq!(session.entries().len(), 2);
    Ok(())
}

#[test]
fn composition_change_requires_a_new_session() -> Result<()> {
    let mut session = Session::new("fixture", "model");
    let mut config = core_config();
    bind_session_composition(&mut session, &config)?;
    config
        .composition
        .enhancements
        .insert(OptionalCapability::Terminal);
    assert!(validate_session_composition(&session, &config).is_err());
    assert!(bind_session_composition(&mut session, &config).is_err());
    assert_eq!(session.entries().len(), 1);
    Ok(())
}

#[test]
fn unbound_execution_history_is_not_silently_migrated() -> Result<()> {
    let mut session = Session::new("fixture", "model");
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("old answer".to_owned()),
        vec![],
    ))?;
    assert!(bind_session_composition(&mut session, &core_config()).is_err());
    assert_eq!(session.entries().len(), 2);
    Ok(())
}

#[test]
fn duplicate_bindings_are_rejected_even_when_identical() -> Result<()> {
    let mut session = Session::new("fixture", "model");
    let config = core_config();
    bind_session_composition(&mut session, &config)?;
    session.append_control(ControlEntry::SessionCompositionBound(
        SessionCompositionSnapshotV1::new(config.selected_capabilities()),
    ))?;
    assert!(validate_session_composition(&session, &config).is_err());
    Ok(())
}

#[test]
fn binding_cannot_retroactively_authorize_old_execution_history() -> Result<()> {
    let config = core_config();
    let mut session = Session::new("fixture", "model");
    session.append_assistant_message(ModelMessage::assistant(
        Some("old answer".to_owned()),
        vec![],
    ))?;
    session.append_control(ControlEntry::SessionCompositionBound(
        SessionCompositionSnapshotV1::new(config.selected_capabilities()),
    ))?;
    assert!(validate_session_composition(&session, &config).is_err());
    Ok(())
}

#[test]
fn child_session_inherits_selection_and_rejects_different_existing_selection() -> Result<()> {
    let config = core_config();
    let mut parent = Session::new("fixture", "model");
    bind_session_composition(&mut parent, &config)?;
    let mut child = Session::new("fixture", "model");
    inherit_session_composition(&parent, &mut child)?;
    validate_session_composition(&child, &config)?;
    let mut other_config = config;
    other_config
        .composition
        .enhancements
        .insert(OptionalCapability::Terminal);
    let mut existing = Session::new("fixture", "model");
    bind_session_composition(&mut existing, &other_config)?;
    assert!(inherit_session_composition(&parent, &mut existing).is_err());
    Ok(())
}
