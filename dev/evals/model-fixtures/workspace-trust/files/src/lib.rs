pub fn value() -> u32 {
    4
}

#[cfg(test)]
mod tests {
    use super::value;

    #[test]
    fn has_expected_value() {
        assert_eq!(value(), 5);
    }
}
