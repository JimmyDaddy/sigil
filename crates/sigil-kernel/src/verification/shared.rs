use super::*;

pub(super) fn stable_hash_parts<'a>(
    check_spec_id: &'a str,
    command: &'a str,
    args: impl IntoIterator<Item = &'a str>,
    cwd: &'a str,
    scope_hash: &'a str,
    effect: &'a str,
) -> String {
    let mut digest = Sha256::new();
    for part in [check_spec_id, command] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    for arg in args {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    for part in [cwd, scope_hash, effect] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}
