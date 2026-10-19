# Immutable owner inputs

This is a provisioning fixture, not the current Gate model or a replacement for
its owner draft. It makes the original inputs available to unattended clones
without owner checkout mounts, host access, Git remotes, or owner completion.

## Identity and reproduction

`model.tar.gz` is the **complete tracked tree**, including every import,
campaign, configuration, documentation input and owned runner, from
`e02a7fd7acb8e2fe5b6a985dc08e3dd2c25cbcb5`, whose parent is
`1de4822893665f32b3beb0b1723d0d27b5908298`. No files or model semantics were
rewritten. Reproduce it in a repository containing that checkpoint:

```bash
git archive e02a7fd7acb8e2fe5b6a985dc08e3dd2c25cbcb5 | gzip -n > model.tar.gz
sha256sum model.tar.gz
```

SHA256: `92c2bd871227acc7df0a0237e47357c30acebac1fadc408b8bb25f1151907d60`. Nix
checks this hash before unpacking the tree into a read-only store output. The
compressed complete tree avoids importing historical specs into current spec
discovery and keeps the original runner's transitive identity inputs.

`rpc/tools.nix` and `rpc/LoomApalache.java` are byte-for-byte exports of
`nix/quint/{tools.nix,LoomApalache.java}` from bounded RPC implementation
`fef01065313be0d7cb2b740d61844defae9b81d4`. Their SHA256 values are respectively
`95d0cce2e5f958ea4130831d2ca09b9bfa913acb000002b836392ca9b15f47e3` and
`a67c5a6bdee032d564132dd0e1430fd2f90fd79b583238dbadf63bb02261c8f6`. The normal
project lock supplies Quint 0.33.0 / evaluator 0.7.0 / Apalache 0.62.1. The
original bounded launcher retains its finite JVM, JSON, and process limits. It
is scoped to this admission fixture; ordinary model execution is unchanged.

## Unattended consumption

No private refs, bundles, local safety history or originating workspace are
needed after cloning this change:

```bash
nix build .#quint-owner-inputs .#quint-owner-admission --no-link
nix path-info .#quint-owner-inputs
nix run .#quint-owner-admission
nix build .#checks.x86_64-linux.quint-owner-admission --no-link
nix run .#test-quint -- provisioning-and-batching
```

The admission command invokes the original `scripts/quint-symbolic.sh` with
scenario 0, `symbolicInitAt(0)`, `symbolicStep`, `safety`, original
configuration, and the unchanged campaign's depth 10 / 2048 MiB / 2400-second
overall budget. The bounded RPC launcher also retains its original 300-second
process ceiling. Tools are built before execution, not downloaded per trace. The
existing provisioning-and-batching verifier checks availability of this complete
input set and bounded solver before its existing batched campaigns. It does not
reinterpret input availability as symbolic success.

## Observed result and assurance

Unattended invocation reaches Apalache but exits 1: the original configuration
contains `checker.smt-encoding = {"type":"oopsla-19"}`, while the pinned solver
expects a JSON string. The owned runner reports `stage=verifying`,
`status=failure`, `errors=[]`; the wrapper retains a compact `incomplete`
receipt and exits nonzero. To see the underlying diagnostic, run the same
`quint verify` invocation without `--out`, against an owned bounded server:
`$.checker.smt-encoding: Expected a JSON string.`

The regression check expects that rejection, never a symbolic pass. This proves
retrieval and invocation of the original bytes through the bounded RPC
implementation, **not** semantic certification, a completed symbolic campaign,
implementation conformance, publication permission or owner-task completion.
Correction belongs to the model owner; the immutable fixture must not be
silently edited to hide this newly reachable failure.
