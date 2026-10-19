set -euo pipefail

# Whole-gate model alignment

    judge_model_alignment() {
      judge_files \
        "specs/simulation.md" \
        "specs/verify.md" \
        "specs/evidence.md" \
        "specs/gate.md" \
        "specs/loop.md" \
        "specs/pre-commit.md" \
        "specs/specs.md" \
        "specs/findings.md" \
        "specs/gate/README.md" \
        "specs/gate/model.qnt" \
        "specs/gate/witness.qnt" \
        "specs/gate/campaign.json" \
        "scripts/test-quint-model.sh" \
        "tests/quint/model-harness.sh" \
        "nix/quint/tools.nix" \
        "nix/quint/checker.rs"
      judge_criterion \
        "Trace model behavior against the owning contracts, not keywords or passing receipts. The reference must independently discover and derive each obligation, its inputs and applicability, without optimizer plans, cached targets or shared construction/selection helpers. Stable deterministic coverage must compare per obligation; effectful observations require current admissibility rather than identical rerun outcomes. Examine all tiers, callers, hook ordering, criterion-local deferral with shared units, canonical identity versus mutable trust/history, pending readiness, candidate-to-resolved task purpose, broken-acceptance repair, retained cross-campaign counterexamples, crash-safe journal/object/checkpoint recovery, worker isolation, same-attempt handoff, ended attempts and unknown remote outcomes. Positive witnesses must reach publication, reuse, feedback, tree inspection, rehearsal and repaired dispatch; forbidden actions must execute and expose rejection. Check that every declared case runs and that exhaustive-checking restrictions, bounds, seeds, tool identities and trust assumptions are explicit. Verify that the Rust checker consumes Quint-compiled IR, enforces the complete boundedInit/step choice-domain contract, rejects hidden nondeterminism, and does not replace exhaustive search with sampling or a duplicate transition model. A bounded frontier is not unrestricted interleaving coverage, and model success is neither implementation conformance nor pilot adoption. Flat owner contracts remain authoritative; this support vocabulary must not become a production adapter schema or duplicate stage-policy authority. Changes to English meaning require renewed alignment review even when model evidence is unchanged. Report concrete missing sequences, circular oracle dependencies, unjustified assumptions or weakened obligations, citing applicable style/spec rules. Do not require production conformance or portable fix replay work to be implemented by this model-only change. When reviewing an actual confirmed-counterexample fix, inspect its checked-in replay against the recorded defect and reject an omitted or unrelated regression under the ordinary review policy."
    }
