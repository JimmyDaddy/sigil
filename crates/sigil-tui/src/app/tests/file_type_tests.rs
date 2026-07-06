use super::*;

#[test]
fn file_type_helpers_cover_languages_and_preview_classes() {
    assert_eq!(
        path_language("src/main.rs".to_owned()),
        Some("rust".to_owned())
    );
    assert_eq!(
        path_language("Dockerfile".to_owned()),
        Some("dockerfile".to_owned())
    );
    assert_eq!(path_language("README.unknown".to_owned()), None);

    assert!(path_has_document_extension("README.md"));
    assert!(path_has_document_extension("guide.adoc"));
    assert!(!path_has_document_extension("src/lib.rs"));

    assert!(path_has_code_or_data_extension("src/lib.rs"));
    assert!(path_has_code_or_data_extension("Dockerfile"));
    assert!(!path_has_code_or_data_extension("notes.txt"));
}
