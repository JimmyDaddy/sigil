use super::Damage;

#[test]
fn damage_union_is_idempotent_and_preserves_all_causes() {
    let merged = Damage::INPUT
        .union(Damage::HOST_EFFECT)
        .union(Damage::ASYNC);
    assert!(!merged.is_empty());
    assert_eq!(merged.union(Damage::INPUT), merged);
    assert_eq!(Damage::NONE.union(merged), merged);
}
