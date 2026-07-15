use std::path::Path;

use super::*;

#[test]
fn repo_language_registry_maps_the_frozen_v1_extensions() {
    let cases = [
        ("src/lib.rs", RepoLanguage::Rust),
        ("app/main.py", RepoLanguage::Python),
        ("app/types.pyi", RepoLanguage::Python),
        ("web/app.js", RepoLanguage::JavaScript),
        ("web/view.jsx", RepoLanguage::JavaScript),
        ("web/module.mjs", RepoLanguage::JavaScript),
        ("web/config.cjs", RepoLanguage::JavaScript),
        ("web/app.ts", RepoLanguage::TypeScript),
        ("web/module.mts", RepoLanguage::TypeScript),
        ("web/config.cts", RepoLanguage::TypeScript),
        ("web/view.tsx", RepoLanguage::Tsx),
        ("cmd/main.go", RepoLanguage::Go),
    ];

    for (path, expected) in cases {
        assert_eq!(repo_language_for_path(Path::new(path)), Some(expected));
    }
    assert_eq!(repo_language_for_path(Path::new("README.md")), None);
    assert_eq!(
        repo_language_for_path(Path::new("src/lib.RS")),
        Some(RepoLanguage::Rust)
    );
}

#[test]
fn repo_language_adapters_extract_multilingual_definitions_and_references() {
    let fixtures = [
        (
            RepoLanguage::Rust,
            "src/lib.rs",
            "pub fn target() {}\nfn caller() { target(); }\n",
            "target",
            true,
        ),
        (
            RepoLanguage::Python,
            "app/main.py",
            "def target():\n    pass\n\ndef caller():\n    target()\n",
            "target",
            true,
        ),
        (
            RepoLanguage::JavaScript,
            "web/app.js",
            "function target() {}\nfunction caller() { target(); }\n",
            "target",
            true,
        ),
        (
            RepoLanguage::JavaScript,
            "web/view.jsx",
            "export function View() { return <Panel />; }\n",
            "View",
            false,
        ),
        (
            RepoLanguage::TypeScript,
            "web/app.ts",
            "function target(): void {}\nfunction caller(): void { target(); }\n",
            "target",
            true,
        ),
        (
            RepoLanguage::Tsx,
            "web/view.tsx",
            "export function View(): JSX.Element { return <Panel />; }\n",
            "View",
            false,
        ),
        (
            RepoLanguage::Go,
            "cmd/main.go",
            "package main\nfunc target() {}\nfunc caller() { target() }\n",
            "target",
            true,
        ),
    ];
    let workspace = Path::new("/workspace");

    for (language, relative, source, expected_definition, expect_reference) in fixtures {
        let tags = extract_repo_tags(
            language,
            workspace,
            &workspace.join(relative),
            source,
            32,
            32,
        )
        .unwrap_or_else(|error| panic!("{} adapter failed: {error:#}", language.as_str()));
        assert!(
            tags.definitions
                .iter()
                .any(|definition| definition.name == expected_definition),
            "{} definitions were {:?}",
            language.as_str(),
            tags.definitions
        );
        if expect_reference {
            assert!(
                tags.references
                    .iter()
                    .any(|reference| reference.name == "target"),
                "{} references were {:?}",
                language.as_str(),
                tags.references
            );
        }
    }
}

#[test]
fn repo_language_tag_caps_are_applied_after_deterministic_sorting() {
    let source = "function zeta() {}\nfunction alpha() {}\nzeta();\nalpha();\n";
    let workspace = Path::new("/workspace");
    let path = workspace.join("web/app.js");

    let tags = extract_repo_tags(RepoLanguage::JavaScript, workspace, &path, source, 1, 1)
        .expect("javascript tags");

    assert_eq!(tags.definitions.len(), 1);
    assert_eq!(tags.references.len(), 1);
    assert_eq!(tags.definitions[0].name, "zeta");
    assert_eq!(tags.references[0].name, "zeta");
}
