# Finite-review semantic probes

These cases distinguish lost obligations from preserved coverage. Evaluate the
actual delivered review guidance against the evidence, not matching words.

## Uncovered billing after a discovery exemption

Unchanged `specs/billing.md` requires invoice totals to round ties to even and
refunds to preserve signed cents. Its acceptance points to `billing_rounding`. A
changed shared discovery helper previously returned `billing` and `shipping`; it
now excludes `billing` as an expensive integration fixture. The residual
shipping tests pass. No contract authorizes retiring billing obligations.

Before:

```rust
fn discovered_subjects() -> Vec<Subject> {
    vec![Subject::Billing, Subject::Shipping]
}
```

After:

```rust
fn discovered_subjects() -> Vec<Subject> {
    vec![Subject::Shipping]
}
```

Expected judgment: ordinary coverage finding against billing's existing
acceptance anchor. Losing the billing subject loses both invoice and refund
obligations, even without an annotation edit or a bond to billing.

## Replacement owner, demonstrated versus asserted

The unchanged billing contract retains the same two obligations. The change
removes `billing_rounding` with a claim that the new ledger owner supersedes it.

Demonstrated replacement:

```rust
#[test]
fn ledger_preserves_invoice_and_refund_cents() {
    let ledger = Ledger::open_in_memory().unwrap();
    ledger.invoice("customer", Decimal::new(1005, 3)).unwrap();
    ledger.refund("customer", Decimal::new(1005, 3)).unwrap();
    assert_eq!(ledger.entries()[0].cents(), 100);
    assert_eq!(ledger.entries()[1].cents(), -100);
    assert_eq!(ledger.balance("customer"), 0);
}
```

Acceptance now references this real ledger path, which executes the production
rounding code on both signed subjects. Expected judgment: no coverage-loss
finding if the live implementation and execution evidence confirm these claims.

Counterexample replacement:

```rust
#[test]
fn ledger_opens() {
    assert!(Ledger::open_in_memory().is_ok());
}
```

Expected judgment: the supersedes claim is not demonstrated; invoice rounding
and signed refunds are still uncovered. A new owner or test name is not proof.

## README input removal: responsibility determines the answer

Before both checkers declare `README.md` among their tracked inputs. The change
removes that input from each checker; current README contents do not change.

Code-only checker:

```text
obligation: reject use of panicking macros in production Rust
subjects: production Rust modules
inputs after: Cargo.toml, clippy.toml, crates/**/src/**/*.rs
```

Expected judgment: dependency precision, no lost subject or obligation. README
is not consumed, discovered membership and real production checks remain intact,
and relevant new source membership remains tracked.

Index checker:

```text
obligation: README's documented CLI commands agree with the spec index
subjects: README.md, current indexed specs
inputs after: specs/**
```

Expected judgment: coverage finding. A future README edit can introduce a stale
command without affecting selection or evidence; current green execution does
not establish coverage of the removed input responsibility. Accept a replacement
only if another demonstrated owner checks that same README/spec agreement.
