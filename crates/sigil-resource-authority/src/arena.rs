//! RFC-0071 sections 7.4 / 10.6: managed arenas and ExecutionTemp standard layout.
//!
//! ExecutionTemp and SessionScratch are separate physical namespaces. They are never aliased:
//! SIGIL_SCRATCH_DIR points only to a SessionScratch generation when the approved requirement
//! explicitly includes it; TMPDIR/TMP/TEMP/HOME/XDG_*_HOME/SIGIL_*_HOME always map to the
//! ExecutionTemp layout. Allocation is atomic: the leaf exists only after the arena + quota
//! reservation are both durable, and generation names never collide.

use std::path::{Path, PathBuf};

use sigil_kernel::resource::CanonicalHash;

/// ExecutionTemp standard layout (logical relative names are fixed; host path varies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTempLayoutV1 {
    pub tmp: String,
    pub home: String,
    pub state: String,
    pub cache: String,
    pub sigil_state: String,
    pub sigil_cache: String,
    pub config: String,
}

impl Default for ExecutionTempLayoutV1 {
    fn default() -> Self {
        Self {
            tmp: "tmp".to_owned(),
            home: "home".to_owned(),
            state: "state".to_owned(),
            cache: "cache".to_owned(),
            sigil_state: "sigil-state".to_owned(),
            sigil_cache: "sigil-cache".to_owned(),
            config: "config".to_owned(),
        }
    }
}

/// Environment mapping contract: which env var -> which layout relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionTempEnvMappingV1 {
    Tmpdir,
    Home,
    XdgStateHome,
    XdgCacheHome,
    SigilStateHome,
    SigilCacheHome,
}

impl ExecutionTempEnvMappingV1 {
    pub const fn var_name(self) -> &'static str {
        match self {
            Self::Tmpdir => "TMPDIR",
            Self::Home => "HOME",
            Self::XdgStateHome => "XDG_STATE_HOME",
            Self::XdgCacheHome => "XDG_CACHE_HOME",
            Self::SigilStateHome => "SIGIL_STATE_HOME",
            Self::SigilCacheHome => "SIGIL_CACHE_HOME",
        }
    }

    pub const fn layout_path(self) -> &'static str {
        match self {
            Self::Tmpdir => "tmp",
            Self::Home => "home",
            Self::XdgStateHome => "state",
            Self::XdgCacheHome => "cache",
            Self::SigilStateHome => "sigil-state",
            Self::SigilCacheHome => "sigil-cache",
        }
    }
}

/// Resolved physical layout for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTempRootV1 {
    pub attempt_id: String,
    pub generation: u64,
    pub root: PathBuf,
    pub layout_hash: CanonicalHash,
}

impl ExecutionTempRootV1 {
    /// Stable host-path-free logical layout digest.
    pub fn layout_digest(&self) -> CanonicalHash {
        arena_digest(b"execution-temp-layout-v1")
    }

    /// Resolves one mapped env var to its physical subdirectory.
    pub fn resolve_env_dir(&self, mapping: ExecutionTempEnvMappingV1) -> PathBuf {
        self.root.join(mapping.layout_path())
    }
}

/// Closed arena allocation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArenaErrorV1 {
    #[error("arena root is not a plain directory: {0}")]
    NotPlainDirectory(String),
    #[error("attempt generation already exists: {0}")]
    GenerationCollision(String),
    #[error("ExecutionTemp and SessionScratch must never resolve to the same directory")]
    TempScratchAlias,
    #[error("host-private diagnostics may not live under a child-granted ExecutionTemp root")]
    DiagnosticsUnderGrant,
}

/// `ExecutionTemp/<attempt-id>/<generation>/` physical scope.
pub fn execution_temp_root(temp_base: &Path, attempt_id: &str, generation: u64) -> PathBuf {
    temp_base.join(attempt_id).join(generation.to_string())
}

/// `SessionScratch/<session-id>/<generation>/data` physical scope.
pub fn session_scratch_root(scratch_base: &Path, session_id: &str, generation: u64) -> PathBuf {
    scratch_base
        .join(session_id)
        .join(generation.to_string())
        .join("data")
}

/// Aliasing guard: the two physical roots may never be equal or ancestor/descendant.
pub fn assert_no_temp_scratch_alias(temp: &Path, scratch: &Path) -> Result<(), ArenaErrorV1> {
    let a = lexically_normalize(temp);
    let b = lexically_normalize(scratch);
    if a == b || a.starts_with(&b) || b.starts_with(&a) {
        return Err(ArenaErrorV1::TempScratchAlias);
    }
    Ok(())
}

fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn arena_digest(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_execution_temp_and_session_scratch_never_alias() {
        let temp = Path::new("/tmp/sigil-et");
        let scratch = Path::new("/tmp/sigil-et");
        let err = assert_no_temp_scratch_alias(temp, scratch).expect_err("same dir must fail");
        assert!(matches!(err, ArenaErrorV1::TempScratchAlias));

        let scratch2 = Path::new("/tmp/sigil-et/session");
        let err2 = assert_no_temp_scratch_alias(temp, scratch2).expect_err("nested must fail");
        assert!(matches!(err2, ArenaErrorV1::TempScratchAlias));

        let ok = assert_no_temp_scratch_alias(
            Path::new("/tmp/sigil-et"),
            Path::new("/tmp/sigil-scratch"),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn r71_env_mapping_contract_is_closed() {
        assert_eq!(ExecutionTempEnvMappingV1::Tmpdir.var_name(), "TMPDIR");
        assert_eq!(ExecutionTempEnvMappingV1::Home.var_name(), "HOME");
        assert_eq!(
            ExecutionTempEnvMappingV1::SigilCacheHome.var_name(),
            "SIGIL_CACHE_HOME"
        );
        assert_eq!(
            ExecutionTempEnvMappingV1::XdgStateHome.layout_path(),
            "state"
        );
    }

    #[test]
    fn r71_execution_temp_root_is_attempt_generation_scoped() {
        let root = execution_temp_root(Path::new("/base"), "attempt-1", 3);
        let expected = format!(
            "base{}attempt-1{}3",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert!(root.ends_with(&expected));
    }
}
