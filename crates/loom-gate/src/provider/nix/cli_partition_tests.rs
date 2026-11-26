#![cfg(test)]

use super::{Cases, Membership, Name, SYSTEM_CASE, reconcile};

const CLI: &str = "loom-verification::cli$storage_keeps_units";
const INDEPENDENT: &str = "loom-events::loom_events$serde_round_trip";

fn name(value: &str) -> Name {
    Name::try_from(value.to_owned()).unwrap()
}

fn case(binary: &str, ignored: bool) -> Membership {
    Membership {
        binary: binary.to_owned(),
        ignored,
        runnable: true,
        default: true,
    }
}

fn inventories() -> (Cases, Cases) {
    let all = Cases::from([
        (name(CLI), case("loom-verification::cli", false)),
        (name(SYSTEM_CASE), case("loom::integration", false)),
        (name(INDEPENDENT), case("loom-events", true)),
    ]);
    let rest = Cases::from([
        (name(SYSTEM_CASE), case("loom::integration", false)),
        (name(INDEPENDENT), case("loom-events", true)),
    ]);
    (all, rest)
}

#[test]
fn closed_partition_keeps_the_outside_runtime_consumer_due() {
    let (all, rest) = inventories();
    assert_eq!(
        reconcile(&all, &rest).unwrap(),
        [name(CLI), name(SYSTEM_CASE)].into()
    );
}

#[test]
fn catalogue_holes_and_filter_changes_cannot_discharge_the_complement() {
    let (all, mut rest) = inventories();
    rest.get_mut(&name(INDEPENDENT)).unwrap().default = false;
    assert!(reconcile(&all, &rest).is_err());
    rest.remove(&name(INDEPENDENT));
    assert!(reconcile(&all, &rest).is_err());
}

#[test]
fn ignored_due_case_and_extra_complement_case_cannot_discharge_the_suite() {
    let (mut all, mut rest) = inventories();
    rest.insert(name("other::binary$unexpected"), case("other", false));
    assert!(reconcile(&all, &rest).is_err());
    rest.remove(&name("other::binary$unexpected"));
    all.get_mut(&name(CLI)).unwrap().ignored = true;
    assert!(reconcile(&all, &rest).is_err());
}
