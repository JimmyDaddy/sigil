use super::*;

pub(super) fn canonical_root(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

pub(super) fn root_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned()
}

pub(super) fn file_uri(path: &std::path::Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let normalized = absolute.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{}", percent_encode_uri_path(&normalized))
    } else {
        format!("file:///{}", percent_encode_uri_path(&normalized))
    }
}

pub(super) fn percent_encode_uri_path(path: &str) -> String {
    let mut output = String::new();
    for byte in path.bytes() {
        let keep =
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'.' | b'_' | b'~');
        if keep {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
