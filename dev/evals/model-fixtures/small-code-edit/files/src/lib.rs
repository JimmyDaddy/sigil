pub fn multiply(left: u32, right: u32) -> u32 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::multiply;

    #[test]
    fn multiplies_values() {
        assert_eq!(multiply(3, 4), 12);
    }
}
