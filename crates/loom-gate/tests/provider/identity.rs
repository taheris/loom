use loom_gate::inventory::snapshot::Identity;
use loom_gate::provider::Digest;
use loom_test_support::proptest_config;
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn canonical_content_and_snapshot_identities_round_trip(bytes in any::<[u8; 32]>()) {
        let canonical = blake3::Hash::from_bytes(bytes).to_hex().to_string();
        let digest = Digest::try_from(canonical.clone()).unwrap();
        let identity = Identity::try_from(canonical.clone()).unwrap();
        prop_assert_eq!(serde_json::to_value(&digest).unwrap(), serde_json::json!(canonical));
        prop_assert_eq!(serde_json::to_value(&identity).unwrap(), serde_json::json!(canonical));
        prop_assert_eq!(serde_json::from_value::<Digest>(serde_json::json!(canonical)).unwrap(), digest);
        prop_assert_eq!(serde_json::from_value::<Identity>(serde_json::json!(canonical)).unwrap(), identity);
    }

    #[test]
    fn noncanonical_identity_casing_cannot_be_normalized(bytes in any::<[u8; 32]>()) {
        let canonical = blake3::Hash::from_bytes(bytes).to_hex().to_string();
        let uppercase = canonical.to_uppercase();
        prop_assume!(uppercase != canonical);
        prop_assert!(Digest::try_from(uppercase.clone()).is_err());
        prop_assert!(Identity::try_from(uppercase).is_err());
    }

    #[test]
    fn truncated_identities_never_become_valid_tokens(text in "[a-z0-9]{0,63}") {
        prop_assert!(Digest::try_from(text.clone()).is_err());
        prop_assert!(Identity::try_from(text).is_err());
    }
}
