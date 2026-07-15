pub fn answer() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::answer;

    #[test]
    fn has_expected_answer() {
        assert_eq!(answer(), 2);
    }
}
