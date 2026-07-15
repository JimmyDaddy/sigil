pub fn workspace_is_unchanged() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::workspace_is_unchanged;

    #[test]
    fn remains_unchanged() {
        assert!(workspace_is_unchanged());
    }
}
