#[cfg(test)]
mod tests {
    #[test]
    fn readme_uses_expected_spelling() {
        assert!(include_str!("../README.md").contains("reliable"));
    }
}
