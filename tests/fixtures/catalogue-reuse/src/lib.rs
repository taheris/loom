#[cfg(test)]
mod tests {
    #[test]
    fn plain_case() {
        assert_eq!(std::hint::black_box(2), 2);
    }

    #[test]
    fn grouped_case() {
        assert_eq!(std::hint::black_box(3), 3);
    }

    #[test]
    #[ignore]
    fn ignored_bad() {
        panic!("real ignored assertion failure");
    }

    #[test]
    #[cfg(feature = "alpha")]
    fn alpha_case() {
        assert!(std::hint::black_box(true));
    }

    #[test]
    #[cfg(feature = "optional_beta")]
    fn optional_dependency_feature_case() {
        assert!(std::hint::black_box(true));
    }

    #[test]
    #[cfg(feature = "beta")]
    fn beta_case() {
        assert!(std::hint::black_box(true));
    }
}
