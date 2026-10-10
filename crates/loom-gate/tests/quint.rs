#[path = "quint/bridge.rs"]
mod bridge;
#[path = "quint/campaign.rs"]
mod campaign;

#[test]
fn production_bridge_has_upstream_driver_state_contract() {
    use quint_connect::State;
    let adapter = bridge::Adapter::default();
    let observation = bridge::Observation::from_driver(&adapter).unwrap();
    assert_eq!(
        observation,
        bridge::Observation {
            count: 0,
            accepted: false
        }
    );
}

#[test]
fn acceptance_runner_preserves_input_read_errors() {
    let root = tempfile::tempdir().unwrap();
    let result = campaign::run(
        root.path(),
        campaign::Mode::Routine,
        bridge::Adapter::default,
    );
    assert!(matches!(result, Err(campaign::Error::Input { .. })));
}
