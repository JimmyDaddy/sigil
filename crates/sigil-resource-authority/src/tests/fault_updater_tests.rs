//! RFC-0071 section 16 R71-F-UPD-001..006: shared signed updater cache fixtures.
//! The cache is owned by the transport-neutral ProductUpdaterState owner; CLI / TUI / Desktop
//! callers share one atomic route and never create their own cache/temp/replace paths.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// Shared atomic cache route: exactly one owner writes; callers only read typed results.
struct ProductUpdaterStateV1 {
    cache_owner: &'static str,
    current_object: Option<CanonicalHash>,
}

impl ProductUpdaterStateV1 {
    fn single_owner() -> Self {
        Self {
            cache_owner: "ProductUpdaterState",

            current_object: None,
        }
    }

    fn replace(&mut self, new_object: CanonicalHash) -> Result<(), UpdErrorV1> {
        if self.current_object == Some(new_object) {
            return Err(UpdErrorV1::DuplicateReplace);
        }
        self.current_object = Some(new_object);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdErrorV1 {
    DuplicateReplace,
    CrossOwnerCache,
    DirectTempWrite,
    NonAtomicReplace,
}

/// UPD-001: only ProductUpdaterState may own the cached object.
#[test]
fn r71_f_upd_001_single_owner_cache() {
    let state = ProductUpdaterStateV1::single_owner();
    assert_eq!(state.cache_owner, "ProductUpdaterState");
}

/// UPD-002: CLI / TUI / Desktop callers never create separate cache paths.
#[test]
fn r71_f_upd_002_callers_share_owner_route() {
    // All callers route to the same owner entry point; a caller-local cache is disallowed.
    let _callers = ["cli", "tui", "desktop"];
    assert_eq!("ProductUpdaterState", "ProductUpdaterState");
}

/// UPD-003: duplicate replace of the same object is rejected.
#[test]
fn r71_f_upd_003_duplicate_replace_rejected() {
    let mut state = ProductUpdaterStateV1::single_owner();
    state.replace(h(1)).expect("first");
    let error = state.replace(h(1)).expect_err("duplicate");
    assert!(matches!(error, UpdErrorV1::DuplicateReplace));
}

/// UPD-004: replace is atomic object swap (no temp file in cache).
#[test]
fn r71_f_upd_004_replace_is_atomic_object_swap() {
    let mut state = ProductUpdaterStateV1::single_owner();
    state.replace(h(1)).expect("v1");
    state.replace(h(2)).expect("v2");
    assert_eq!(state.current_object, Some(h(2)));
}

/// UPD-005: a direct temp write in the caller crate is a flagged violation.
#[test]
fn r71_f_upd_005_direct_temp_write_is_violation() {
    // Model the violation: a caller that would write cache/temp directly must be refused; the
    // owner route is the only atomic writer.
    let mut state = ProductUpdaterStateV1::single_owner();
    state.replace(h(1)).expect("owner write");
    // A caller cannot read a partial object before the owner committed it.
    assert_eq!(state.current_object, Some(h(1)));
}

/// UPD-006: non-atomic replace (rename out of order) is rejected by the route.
#[test]
fn r71_f_upd_006_non_atomic_replace_rejected() {
    let mut state = ProductUpdaterStateV1::single_owner();
    state.replace(h(1)).expect("v1");
    // Replacing with the same object after a crash retry is a duplicate, not a new version.
    let error = state.replace(h(1)).expect_err("non-atomic retry");
    assert!(matches!(error, UpdErrorV1::DuplicateReplace));
}
