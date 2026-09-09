use super::*;

fn historical_manifest() -> Schema1Manifest {
    Schema1Manifest {
        schema_version: 1,
        application_instance_id: "sigil:test-predecessor".to_owned(),
        selected_epoch: StartupEpochV1::NewCurrentSchema,
        application_generation: 2,
        authority_generation_digest: CanonicalHash::from_bytes([0x55; 32]),
        mandatory_readiness: SCHEMA_1_ADAPTERS
            .into_iter()
            .map(|adapter| Schema1ReadinessProbe {
                adapter,
                passed: true,
                evidence_digest: CanonicalHash::from_bytes([0x33; 32]),
            })
            .collect(),
        // Golden computed from the historical six-field encoding, before composition existed.
        manifest_hash: serde_json::from_str(
            "\"a5bd93add0210394d3b232f55cbe31413372ff6309f4a4fa6e8651932d005f1d\"",
        )
        .expect("historical hash"),
    }
}

fn encode(manifest: &Schema1Manifest) -> Vec<u8> {
    serde_json::to_vec(manifest).expect("historical bytes")
}

fn rehash(manifest: &mut Schema1Manifest) {
    manifest.manifest_hash = schema1_manifest_hash(manifest);
}

#[test]
fn cutover_predecessor_golden_hash_validates_without_inventing_composition() {
    let historical = historical_manifest();
    assert_eq!(schema1_manifest_hash(&historical), historical.manifest_hash);
    let bytes = encode(&historical);
    let unchanged = bytes.clone();
    let predecessor = validate_bootstrap_cutover_predecessor_v1(&bytes).expect("valid predecessor");
    assert_eq!(
        predecessor.application_instance_id(),
        "sigil:test-predecessor"
    );
    assert_eq!(predecessor.application_generation(), 2);
    assert_eq!(
        predecessor.authority_generation_digest(),
        historical.authority_generation_digest
    );
    assert_eq!(predecessor.manifest_hash(), historical.manifest_hash);
    assert_eq!(bytes, unchanged);
    assert!(
        !String::from_utf8(bytes.clone())
            .expect("utf8")
            .contains("composition")
    );
    assert!(serde_json::from_slice::<super::super::CutoverManifestV1>(&bytes).is_err());
}

#[test]
fn cutover_predecessor_rejects_tampering_and_hash_sensitive_probe_order() {
    let mut manifest = historical_manifest();
    manifest.application_generation += 1;
    assert_eq!(
        validate_bootstrap_cutover_predecessor_v1(&encode(&manifest)),
        Err(CutoverErrorV1::ManifestHashMismatch.into())
    );
    let mut manifest = historical_manifest();
    manifest.mandatory_readiness.swap(0, 1);
    assert_eq!(
        validate_bootstrap_cutover_predecessor_v1(&encode(&manifest)),
        Err(CutoverErrorV1::ManifestHashMismatch.into())
    );
    rehash(&mut manifest);
    validate_bootstrap_cutover_predecessor_v1(&encode(&manifest)).expect("valid rehashed order");
}

#[test]
fn cutover_predecessor_requires_exactly_eighteen_passing_historical_probes() {
    let mut missing = historical_manifest();
    missing.mandatory_readiness.pop();
    rehash(&mut missing);
    assert_eq!(
        validate_bootstrap_cutover_predecessor_v1(&encode(&missing)),
        Err(CutoverErrorV1::MissingReadinessProbe.into())
    );
    let mut duplicate = historical_manifest();
    duplicate.mandatory_readiness.push(Schema1ReadinessProbe {
        adapter: MandatoryAdapterKindV1::ExecutionOneShot,
        passed: true,
        evidence_digest: CanonicalHash::from_bytes([0x33; 32]),
    });
    rehash(&mut duplicate);
    assert_eq!(
        validate_bootstrap_cutover_predecessor_v1(&encode(&duplicate)),
        Err(
            CutoverErrorV1::DuplicateReadinessProbe(MandatoryAdapterKindV1::ExecutionOneShot)
                .into()
        )
    );
    let mut failed = historical_manifest();
    failed.mandatory_readiness[0].passed = false;
    rehash(&mut failed);
    assert_eq!(
        validate_bootstrap_cutover_predecessor_v1(&encode(&failed)),
        Err(CutoverErrorV1::AdapterNotReady(MandatoryAdapterKindV1::ExecutionOneShot).into())
    );
}

#[test]
fn cutover_predecessor_rejects_unknown_versions_extra_fields_and_duplicate_json_keys() {
    let original = String::from_utf8(encode(&historical_manifest())).expect("utf8");
    for version in [0, 2, 99] {
        let bytes = original.replace(
            "\"schema_version\":1",
            &format!("\"schema_version\":{version}"),
        );
        assert_eq!(
            validate_bootstrap_cutover_predecessor_v1(bytes.as_bytes()),
            Err(CutoverErrorV1::UnknownSchemaVersion.into())
        );
    }
    for bytes in [
        original.replacen('{', "{\"composition\":{},", 1),
        original.replacen('{', "{\"schema_version\":1,", 1),
        original.replace("\"passed\":true", "\"passed\":true,\"unrecognized\":1"),
        "{broken".to_owned(),
    ] {
        assert_eq!(
            validate_bootstrap_cutover_predecessor_v1(bytes.as_bytes()),
            Err(CutoverPredecessorErrorV1::InvalidManifest)
        );
    }
}

#[test]
fn cutover_predecessor_rejects_legacy_epoch_and_invalid_identity() {
    for variant in 0..3 {
        let mut manifest = historical_manifest();
        match variant {
            0 => manifest.selected_epoch = StartupEpochV1::Legacy,
            1 => manifest.application_generation = 0,
            _ => manifest.application_instance_id = " ".to_owned(),
        }
        rehash(&mut manifest);
        assert_eq!(
            validate_bootstrap_cutover_predecessor_v1(&encode(&manifest)),
            Err(CutoverPredecessorErrorV1::InvalidManifest)
        );
    }
}
