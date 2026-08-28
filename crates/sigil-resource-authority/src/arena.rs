//! RFC-0071 sections 7.4 / 10.6: managed arenas and ExecutionTemp standard layout.
//!
//! ExecutionTemp and SessionScratch are separate physical namespaces. They are never aliased:
//! SIGIL_SCRATCH_DIR points only to a SessionScratch generation when the approved requirement
//! explicitly includes it; TMPDIR/TMP/TEMP/HOME/XDG_*_HOME/SIGIL_*_HOME always map to the
//! ExecutionTemp layout. Allocation is atomic: the leaf exists only after the arena + quota
//! reservation are both durable, and generation names never collide.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sigil_kernel::{resource::CanonicalHash, secure_private_path_permissions};

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
    #[error("invalid execution attempt identity: {0}")]
    InvalidAttemptId(String),
    #[error("execution-temp filesystem operation failed: {0}")]
    Filesystem(String),
}

/// Resource-authority owner for per-attempt `ExecutionTemp` generations.
///
/// The base is an authority bootstrap anchor. Consumers cannot select a physical generation:
/// this owner derives the exact attempt/generation path, materializes the complete reserved-env
/// layout, hardens every directory before returning it, and is the only cleanup entry point.
#[derive(Debug, Clone)]
pub struct ExecutionTempAuthorityV1 {
    base: PathBuf,
}

impl ExecutionTempAuthorityV1 {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Provisions an exact generation. The returned binding is not cloneable and its root is
    /// published only after all standard env directories exist and are owner-only.
    pub fn provision(
        &self,
        attempt_id: &str,
        generation: u64,
    ) -> Result<ExecutionTempGenerationV1, ArenaErrorV1> {
        validate_attempt_id(attempt_id)?;
        ensure_private_directory(&self.base)?;
        // Attempt ids contain purpose labels and may contain `:`. Keep the host directory
        // portable and opaque by deriving a fixed ASCII component instead of using the id as a
        // filename (notably, `:` is not a valid Windows path component).
        let attempt_root = self.base.join(
            arena_digest(format!("execution-temp-attempt-v1\0{attempt_id}").as_bytes()).to_hex(),
        );
        ensure_private_directory(&attempt_root)?;
        let root = attempt_root.join(generation.to_string());
        if fs::symlink_metadata(&root).is_ok() {
            return Err(ArenaErrorV1::GenerationCollision(
                root.display().to_string(),
            ));
        }
        fs::create_dir(&root).map_err(arena_fs_error)?;
        if let Err(error) = materialize_standard_layout(&root) {
            let _ = fs::remove_dir_all(&root);
            let _ = fs::remove_dir(&attempt_root);
            return Err(error);
        }
        Ok(ExecutionTempGenerationV1 {
            root: ExecutionTempRootV1 {
                attempt_id: attempt_id.to_owned(),
                generation,
                root,
                layout_hash: arena_digest(b"execution-temp-layout-v1"),
            },
            attempt_root,
        })
    }
}

/// Non-clone physical generation binding. Callers must settle it explicitly after process
/// settlement so receipts never claim `Released` before the owned tree has actually gone away.
#[derive(Debug)]
pub struct ExecutionTempGenerationV1 {
    root: ExecutionTempRootV1,
    attempt_root: PathBuf,
}

impl ExecutionTempGenerationV1 {
    #[must_use]
    pub fn binding(&self) -> &ExecutionTempRootV1 {
        &self.root
    }

    pub fn finalize(self) -> Result<(), ArenaErrorV1> {
        let metadata = fs::symlink_metadata(&self.root.root).map_err(arena_fs_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ArenaErrorV1::NotPlainDirectory(
                self.root.root.display().to_string(),
            ));
        }
        // `remove_dir_all` does not follow directory symlinks. The owner-only anchor prevents a
        // different OS user from replacing ancestors, while unconfined same-user execution is
        // reported truthfully by the sandbox receipt rather than treated as confinement.
        fs::remove_dir_all(&self.root.root).map_err(arena_fs_error)?;
        match fs::remove_dir(&self.attempt_root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(arena_fs_error(error)),
        }
    }
}

fn validate_attempt_id(attempt_id: &str) -> Result<(), ArenaErrorV1> {
    if attempt_id.is_empty()
        || attempt_id == "."
        || attempt_id == ".."
        || attempt_id
            .chars()
            .any(|value| value == '/' || value == '\\' || value == '\0')
    {
        return Err(ArenaErrorV1::InvalidAttemptId(attempt_id.to_owned()));
    }
    Ok(())
}

fn materialize_standard_layout(root: &Path) -> Result<(), ArenaErrorV1> {
    secure_private_path_permissions(root)
        .map_err(|error| ArenaErrorV1::Filesystem(error.to_string()))?;
    let layout = ExecutionTempLayoutV1::default();
    for relative in [
        layout.tmp,
        layout.home,
        layout.state,
        layout.cache,
        layout.sigil_state,
        layout.sigil_cache,
        layout.config,
    ] {
        ensure_private_directory(&root.join(relative))?;
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), ArenaErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(ArenaErrorV1::NotPlainDirectory(path.display().to_string()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(arena_fs_error)?;
        }
        Err(error) => return Err(arena_fs_error(error)),
    }
    secure_private_path_permissions(path)
        .map_err(|error| ArenaErrorV1::Filesystem(error.to_string()))
}

fn arena_fs_error(error: std::io::Error) -> ArenaErrorV1 {
    ArenaErrorV1::Filesystem(error.to_string())
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
#[path = "tests/arena_tests.rs"]
mod tests;
