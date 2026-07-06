use std::path::Path;

pub(super) fn path_language(path: String) -> Option<String> {
    path_extension(&path)
        .and_then(|extension| match extension.as_str() {
            "rs" => Some("rust"),
            "toml" | "lock" => Some("toml"),
            "json" | "jsonl" => Some("json"),
            "yaml" | "yml" => Some("yaml"),
            "js" | "jsx" => Some("javascript"),
            "ts" | "tsx" => Some("typescript"),
            "py" => Some("python"),
            "go" => Some("go"),
            "java" => Some("java"),
            "kt" | "kts" => Some("kotlin"),
            "c" | "h" => Some("c"),
            "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
            "cs" => Some("c#"),
            "swift" => Some("swift"),
            "rb" => Some("ruby"),
            "php" => Some("php"),
            "sh" | "bash" | "zsh" | "fish" => Some("bash"),
            "sql" => Some("sql"),
            "html" => Some("html"),
            "css" | "scss" | "sass" => Some("css"),
            "xml" | "svg" => Some("xml"),
            "lua" => Some("lua"),
            "vim" => Some("vim"),
            "dockerfile" => Some("dockerfile"),
            _ => None,
        })
        .map(str::to_owned)
}

pub(super) fn path_has_document_extension(path: &str) -> bool {
    path_extension(path).is_some_and(|extension| {
        matches!(
            extension.as_str(),
            "md" | "markdown" | "mdown" | "mkd" | "rst" | "adoc" | "asciidoc"
        )
    })
}

pub(super) fn path_has_code_or_data_extension(path: &str) -> bool {
    path_extension(path).is_some_and(|extension| {
        matches!(
            extension.as_str(),
            "rs" | "toml"
                | "lock"
                | "json"
                | "jsonl"
                | "yaml"
                | "yml"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "py"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hpp"
                | "cs"
                | "swift"
                | "rb"
                | "php"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "sql"
                | "html"
                | "css"
                | "scss"
                | "sass"
                | "xml"
                | "svg"
                | "lua"
                | "vim"
                | "dockerfile"
        )
    })
}

fn path_extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| name.eq_ignore_ascii_case("Dockerfile"))
                .map(|_| "dockerfile".to_owned())
        })
}

#[cfg(test)]
#[path = "tests/file_type_tests.rs"]
mod tests;
