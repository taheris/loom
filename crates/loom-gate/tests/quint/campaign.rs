// Only the integration suite owns campaign unit tests. The manual example
// compiles the same runtime implementation without duplicating report identities.
include!("campaign_runtime.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::clock::MockClock;
    use loom_test_support::proptest_config;
    use proptest::prelude::*;

    fn budget(plan: &Plan) -> Budget {
        Budget::new(plan, Rc::new(MockClock)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn quint_incomplete_verification_is_not_success() {
        for definition in [
            r#"{"seeds":[],"traces":1,"steps":1,"seconds":1}"#,
            r#"{"seeds":[42,42],"traces":1,"steps":1,"seconds":1}"#,
            r#"{"seeds":[42],"traces":0,"steps":1,"seconds":1}"#,
        ] {
            assert!(serde_json::from_str::<Plan>(definition).is_err());
        }
        let plan = Plan::shipped(Mode::Replay).unwrap();
        let budget = budget(&plan);
        assert!(matches!(budget.finish(), Err(Error::Incomplete { .. })));
        budget.record(true).unwrap();
        assert!(matches!(budget.finish(), Err(Error::Incomplete { .. })));
        assert!(matches!(budget.record(true), Err(Error::Incomplete { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn quint_campaign_budget_exhaustion_is_explicit() {
        let plan = Plan::shipped(Mode::Replay).unwrap();
        let completed = budget(&plan);
        completed.record(true).unwrap();
        completed.record(false).unwrap();
        assert_eq!(completed.finish().unwrap().states, completed.state_limit);
        assert!(matches!(
            completed.record(false),
            Err(Error::Incomplete { .. })
        ));
        let early = Budget {
            state_limit: 1,
            ..budget(&plan)
        };
        early.record(true).unwrap();
        assert!(matches!(early.record(false), Err(Error::Incomplete { .. })));
        assert!(early.finish().is_err());
        let expired = budget(&plan);
        expired.record(true).unwrap();
        tokio::time::advance(Duration::from_secs(plan.seconds())).await;
        assert!(matches!(
            expired.record(false),
            Err(Error::Incomplete { .. })
        ));
        assert!(expired.finish().is_err());
        let routine = Plan::shipped(Mode::Routine).unwrap();
        let deeper = Plan::shipped(Mode::Deeper).unwrap();
        assert_eq!(routine.total_seconds().unwrap(), 60);
        assert!(deeper.steps > routine.steps && deeper.traces > routine.traces);
        assert!(routine.seeds.iter().all(|seed| deeper.seeds.contains(seed)));
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct Shifted(u64);

    struct Custom;

    impl State<Custom> for Shifted {
        fn from_driver(_: &Custom) -> quint_connect::Result<Self> {
            Ok(Self(1))
        }

        fn from_spec(value: itf::Value) -> quint_connect::Result<Self> {
            Ok(Self(u64::deserialize(value)? + 1))
        }
    }

    impl Driver for Custom {
        type State = Shifted;

        fn step(&mut self, _: &Step) -> quint_connect::Result {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn counted_adapter_preserves_custom_upstream_projection() {
        let model =
            <Projection<Shifted> as State<Counted<Custom>>>::from_spec(itf::Value::Number(0))
                .unwrap();
        let driver = Counted {
            driver: Custom,
            budget: Rc::new(budget(&Plan::shipped(Mode::Replay).unwrap())),
        };
        assert_eq!(model, Projection::from_driver(&driver).unwrap());
    }

    proptest! {
        #![proptest_config(proptest_config())]

        #[test]
        fn parsed_campaigns_admit_only_finite_nonempty_unique_batches(
            seeds in proptest::collection::vec(1u64..100, 0..8),
            traces in 0usize..10,
            steps in 0usize..10,
            seconds in 0u64..100,
        ) {
            let definition = serde_json::json!({"seeds": seeds, "traces": traces, "steps": steps, "seconds": seconds});
            let parsed = serde_json::from_value::<Plan>(definition);
            let unique = seeds.iter().collect::<std::collections::BTreeSet<_>>().len() == seeds.len();
            prop_assert_eq!(parsed.is_ok(), !seeds.is_empty() && unique && traces > 0 && steps > 0 && seconds > 0);
        }
    }

    #[test]
    fn seed_change_invalidates_reproducible_identity() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut plan = Plan::shipped(Mode::Routine).unwrap();
        let identity = plan.identity(&root, Mode::Routine).unwrap();
        assert_eq!(identity, plan.identity(&root, Mode::Routine).unwrap());
        plan.seeds[0] = NonZeroU64::new(43).unwrap();
        assert_ne!(identity, plan.identity(&root, Mode::Routine).unwrap());
        assert_ne!(identity, plan.identity(&root, Mode::Deeper).unwrap());
    }
}
