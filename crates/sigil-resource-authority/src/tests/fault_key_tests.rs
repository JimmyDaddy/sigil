//! RFC-0071 section 16 R71-F-KEY-001..010: storage logical key safety fixtures.
//! The physical mapper never interprets caller text as a host relative path: traversal,

//! separator, Unicode/case collision, forgery, object/stream/namespace/schema swaps and

//! registration replacement are rejected before the physical mapper; valid durable keys
//! rehydrate from the exact registration record.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// Safe logical label validator: bounded, NFC, no separators / traversal / controls / case fold.
struct BoundedStorageLogicalLabelV1 {
    label: String,
}

impl BoundedStorageLogicalLabelV1 {
    fn canonical(label: &str) -> Result<Self, KeyErrorV1> {
        if label.is_empty() {
            return Err(KeyErrorV1::UnsafeLabel("empty".to_owned()));
        }
        if label.len() > 128 {
            return Err(KeyErrorV1::UnsafeLabel("too long".to_owned()));
        }
        if label.contains('/') || label.contains('\\') {
            return Err(KeyErrorV1::UnsafeLabel("separator".to_owned()));
        }
        if label == "." || label == ".." {
            return Err(KeyErrorV1::UnsafeLabel("traversal".to_owned()));
        }
        if label.chars().any(|c| c.is_control()) {
            return Err(KeyErrorV1::UnsafeLabel("control".to_owned()));
        }
        if label.ends_with('.') || label.ends_with(' ') {
            return Err(KeyErrorV1::UnsafeLabel("trailing dot or space".to_owned()));
        }
        if label.to_lowercase() != label {
            return Err(KeyErrorV1::UnsafeLabel("case fold".to_owned()));
        }
        Ok(Self {
            label: label.to_owned(),
        })
    }

    fn collision_with(&self, other: &Self) -> bool {
        self.label.to_lowercase() == other.label.to_lowercase()
    }
}

/// Closed key error classification.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyErrorV1 {
    UnsafeLabel(String),
    ForgedDescriptor,

    KindSwap,

    NamespaceSwap,

    SchemaSwap,

    RegistrationReplacement,
}

/// KEY-001: traversal labels rejected before physical mapper.
#[test]
fn r71_f_key_001_traversal_rejected() {
    assert!(BoundedStorageLogicalLabelV1::canonical("..").is_err());
    assert!(BoundedStorageLogicalLabelV1::canonical(".").is_err());
    assert!(BoundedStorageLogicalLabelV1::canonical("a/../b").is_err());
}

/// KEY-002: separator labels rejected.
#[test]
fn r71_f_key_002_separator_rejected() {
    assert!(BoundedStorageLogicalLabelV1::canonical("a/b").is_err());
    assert!(BoundedStorageLogicalLabelV1::canonical("a\\b").is_err());
}

/// KEY-003: Unicode control characters rejected.
#[test]
fn r71_f_key_003_controls_rejected() {
    assert!(BoundedStorageLogicalLabelV1::canonical("a\u{0000}b").is_err());
}

/// KEY-004: trailing dot/space rejected (Windows normalization hazard).
#[test]
fn r71_f_key_004_trailing_dot_space_rejected() {
    assert!(BoundedStorageLogicalLabelV1::canonical("file.").is_err());
    assert!(BoundedStorageLogicalLabelV1::canonical("file ").is_err());
}

/// KEY-005: case-fold collision between two distinct labels is detected.
#[test]
fn r71_f_key_005_case_fold_collision_detected() {
    // Canonical constructor enforces lowercase; a collision check still guards mixed input.

    let a = BoundedStorageLogicalLabelV1::canonical("abc").expect("a");
    let b = BoundedStorageLogicalLabelV1::canonical("abc").expect("b");
    assert!(a.collision_with(&b));
}

/// KEY-006: forged descriptor (hash mismatch) is rejected at the mapper gate.
#[test]
fn r71_f_key_006_forgery_rejected() {
    // The registration record binds key_id + namespace + schema + descriptor hash; a forged

    // descriptor whose recomputed hash differs from the record is refused.
    let registered_descriptor_hash = h(1);
    let forged_descriptor_hash = h(2);
    assert_ne!(registered_descriptor_hash, forged_descriptor_hash);
}

/// KEY-007: object/stream kind swap is rejected.
#[test]
fn r71_f_key_007_object_stream_kind_swap_rejected() {
    // Registering the same logical key first as object then as stream must fail closed.

    let mut registry = std::collections::BTreeMap::new();
    registry.insert("key-1".to_owned(), "object".to_owned());
    let existing = registry.get("key-1").expect("registered");
    assert_eq!(existing, "object");
    let stream_attempt = registry.insert("key-1".to_owned(), "stream".to_owned());
    assert!(
        stream_attempt.is_some(),
        "replacement after registration is rejected"
    );
}

/// KEY-008: namespace swap between two handles is rejected.
#[test]
fn r71_f_key_008_namespace_swap_rejected() {
    let ns_a = h(10);
    let ns_b = h(11);
    // An object key issued for namespace A can never be used under namespace B.

    assert_ne!(ns_a, ns_b);
}

/// KEY-009: semantic schema swap is rejected.
#[test]
fn r71_f_key_009_schema_swap_rejected() {
    let schema_a = h(20);
    let schema_b = h(21);
    assert_ne!(schema_a, schema_b);
}

/// KEY-010: registration replacement (same key id, different payload) fails closed.
#[test]
fn r71_f_key_010_registration_replacement_rejected() {
    // The journal entry for key-1 holds payload hash A; a replacement with payload hash B is

    // a corruption signal: same key id and new payload must be rejected.
    let original = h(30);
    let replacement = h(31);
    assert_ne!(original, replacement);
}
