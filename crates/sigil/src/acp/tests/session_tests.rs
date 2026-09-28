use super::*;

#[test]
fn public_identity_is_bounded_and_never_a_path() -> Result<()> {
    let identity = SessionIdentity {
        nonce: uuid::Uuid::new_v4(),
        scope: "durable-scope".to_owned(),
    };
    let id = identity.public_id();
    let parsed = SessionIdentity::parse(&id)?;
    assert_eq!(parsed.nonce, identity.nonce);
    assert_eq!(parsed.scope, identity.scope);
    for invalid in [
        "/tmp/records.jsonl".to_owned(),
        "sigil-acp-v0:abc:scope".to_owned(),
        format!("sigil-acp-v1:{}:scope", identity.nonce),
        format!("sigil-acp-v1:{}:", identity.nonce.simple()),
        format!(
            "sigil-acp-v1:{}:{}",
            identity.nonce.simple(),
            "x".repeat(257)
        ),
    ] {
        assert!(SessionIdentity::parse(&invalid).is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn host_namespace_is_stable_and_workspace_bound() -> Result<()> {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    let first = first.path().canonicalize()?;
    let second = second.path().canonicalize()?;
    let nonce = uuid::Uuid::new_v4();
    let key = namespace_key(&first, nonce);
    assert_eq!(key, namespace_key(&first, nonce));
    assert_ne!(key, namespace_key(&second, nonce));
    assert_ne!(key, namespace_key(&first, uuid::Uuid::new_v4()));
    assert_eq!(key.len(), 64);
    assert!(key.starts_with("acp-"));
    Ok(())
}
