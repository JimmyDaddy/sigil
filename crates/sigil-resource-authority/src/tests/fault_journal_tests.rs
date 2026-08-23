//! RFC-0071 section 16 R71-F-JRN-001..008: journal fault fixtures.
//! Deterministic cases covering header create, header-only, first BootstrapBound append,
//! duplicate instance/bootstrap swap, zero/invalid genesis and restart chain breaks.

use crate::journal::{
    JournalErrorV1, ResourceJournalAppendPreconditionV1, ResourceJournalEventV1,
    ResourceJournalHeaderV1, ResourceJournalMemoryV1,
};
use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

fn header(instance_seed: u8) -> ResourceJournalHeaderV1 {
    ResourceJournalHeaderV1 {
        schema_version: 1,
        shard_name: "application-resources".to_owned(),
        bootstrap_manifest_hash: h(10),
        journal_instance_hash: h(instance_seed),
        header_hash: h(20 + instance_seed),
    }
}

fn genesis_precondition(j: &ResourceJournalMemoryV1) -> ResourceJournalAppendPreconditionV1 {
    let hd = j.header().expect("header").clone();
    ResourceJournalAppendPreconditionV1::Empty {
        expected_header_hash: hd.header_hash,
        expected_journal_instance_hash: hd.journal_instance_hash,
    }
}

#[test]
fn r71_f_jrn_001_header_create_then_genesis_is_unique_sequence_one() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(1)).expect("header");
    let record = j
        .append(
            &genesis_precondition(&j),
            &ResourceJournalEventV1::BootstrapBound {
                bootstrap_manifest_hash: h(10),
            },
        )
        .expect("genesis");
    assert_eq!(record.sequence, 1);
    let error = j.append(
        &genesis_precondition(&j),
        &ResourceJournalEventV1::BootstrapBound {
            bootstrap_manifest_hash: h(10),
        },
    );
    assert!(error.is_err());
}

#[test]
fn r71_f_jrn_002_header_only_journal_reports_header_with_zero_records() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(2)).expect("header");
    assert!(j.header().is_some());
    assert!(j.tail().is_none(), "header-only journal has no tail");
}

#[test]
fn r71_f_jrn_003_zero_genesis_sequence_is_rejected_before_append() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(3)).expect("header");
    let error = j.append(
        &ResourceJournalAppendPreconditionV1::Existing {
            expected_sequence: 0,
            expected_record_hash: h(99),
            expected_journal_instance_hash: h(3),
        },
        &ResourceJournalEventV1::BootstrapBound {
            bootstrap_manifest_hash: h(10),
        },
    );
    assert!(matches!(error, Err(JournalErrorV1::PreconditionMismatch)));
}

#[test]
fn r71_f_jrn_004_invalid_genesis_payload_fails_closed() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(4)).expect("header");
    let error = j.append(
        &genesis_precondition(&j),
        &ResourceJournalEventV1::GenerationReserved {
            resource_id: "r".to_owned(),
            generation: 1,
        },
    );
    assert!(matches!(
        error,
        Err(JournalErrorV1::FirstRecordNotBootstrapBound)
    ));
}

#[test]
fn r71_f_jrn_005_duplicate_journal_instance_is_rejected() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(5)).expect("first");
    let error = j.install_header(header(5)).expect_err("duplicate");
    assert!(matches!(error, JournalErrorV1::InstanceMismatch));
}

#[test]
fn r71_f_jrn_006_bootstrap_swap_fails_closed() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(6)).expect("header");
    let precondition = ResourceJournalAppendPreconditionV1::Empty {
        expected_header_hash: h(255),
        expected_journal_instance_hash: h(6),
    };
    let error = j.append(
        &precondition,
        &ResourceJournalEventV1::BootstrapBound {
            bootstrap_manifest_hash: h(10),
        },
    );
    assert!(matches!(error, Err(JournalErrorV1::PreconditionMismatch)));
}

#[test]
fn r71_f_jrn_007_restart_replays_from_unique_genesis() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(7)).expect("header");
    j.append(
        &genesis_precondition(&j),
        &ResourceJournalEventV1::BootstrapBound {
            bootstrap_manifest_hash: h(10),
        },
    )
    .expect("genesis");
    let tail = j.tail().expect("tail").clone();
    let next = j
        .append(
            &ResourceJournalAppendPreconditionV1::Existing {
                expected_sequence: tail.sequence,
                expected_record_hash: tail.record_hash,
                expected_journal_instance_hash: h(7),
            },
            &ResourceJournalEventV1::GenerationReserved {
                resource_id: "r".to_owned(),
                generation: 1,
            },
        )
        .expect("next");
    assert_eq!(next.sequence, 2);
}

#[test]
fn r71_f_jrn_008_chain_broken_on_restart_stale_tail_is_rejected() {
    let mut j = ResourceJournalMemoryV1::new();
    j.install_header(header(8)).expect("header");
    j.append(
        &genesis_precondition(&j),
        &ResourceJournalEventV1::BootstrapBound {
            bootstrap_manifest_hash: h(10),
        },
    )
    .expect("genesis");
    let error = j.append(
        &ResourceJournalAppendPreconditionV1::Existing {
            expected_sequence: 999,
            expected_record_hash: h(1),
            expected_journal_instance_hash: h(8),
        },
        &ResourceJournalEventV1::GenerationReserved {
            resource_id: "r".to_owned(),
            generation: 1,
        },
    );
    assert!(matches!(error, Err(JournalErrorV1::PreconditionMismatch)));
}
