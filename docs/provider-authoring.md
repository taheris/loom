# Checked provider API examples

The tracked Rust API binds a named callable, discovered subjects, offered
duties, checker source/build identity and input access in one `Definition`.
`Registry` generates the typed query response and resolves it against the same
definition, current captured repository and independent `Inventory`. `Unit`
cannot be constructed from JSON or have its admitted fields rewritten.

This example is a diagnostic execution path, not a substitute for
`loom gate verify`, inventory-wide coverage, Evidence admission or publication.
The ordinary CLI gate's provider integration is not exposed by these examples.
The APIs also provide inventory-wide admission and a specifically supported
engine-info effect, described below; recognized candidate shapes alone cannot
register arbitrary commands.

## Run the Rust example

From the Loom checkout, create a separate temporary native Cargo project. The
source below is copied from the shipped example, so the registered checker
source is also the code compiled for this project. Cargo supplies native
ownership and lockfile metadata; there is no second input table or hand-authored
description. Initialize and copy Loom's exact dependency patches into the
temporary project before running Cargo; an external Cargo project does not
inherit this checkout's `.cargo/config.toml`.

```bash
set -euo pipefail
loom_root="$PWD"
bash "$loom_root/scripts/configure-cargo-patches.sh"
project=$(mktemp -d)
mkdir -p "$project/src" "$project/specs/example" "$project/docs" "$project/.cargo"
cp "$(readlink -f "$loom_root/.cargo/nix-patches.toml")" "$project/.cargo/config.toml"
printf '# Example contract\n' > "$project/specs/example/spec.md"
printf '%s\n' '- [example](../specs/example/spec.md)' > "$project/docs/README.md"
cp "$loom_root/crates/loom-gate/examples/provider.rs" "$project/src/main.rs"
printf '/target/\n' > "$project/.gitignore"
printf '# Example\n' > "$project/README.md"
printf '# Example\n\n## Success Criteria\n\n- A document has a heading [check](document-heading)\n' > "$project/specs/example/tests.md"
printf '[package]\nname = "provider-example"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\nloom-gate = { path = "%s/crates/loom-gate" }\nanyhow = "1"\nserde_json = "1"\n' "$loom_root" > "$project/Cargo.toml"
git init "$project"
cd "$project"
cargo generate-lockfile --offline
cargo run --offline -- --print-inputs document-heading
cargo run --offline
```

Query stdout is one strict JSON document containing per-target `units`. It is
candidate data, not a result or an assertion that coverage is complete. Normal
execution consumes that generated query through `Registry::resolve_query`, then
prints an observed result with tracked membership/content reads. Supporting
Cargo diagnostics remain on stderr. A valid cold check needs no prior trace.

`Definition::tracked` accepts a **named function item**. Do not coerce it to a
function pointer or replace it with a closure: doing so erases or makes
ambiguous the callable identity. Checker inputs include current native
Rust/configuration sources conservatively, selected analysis subjects and the
owning acceptance contract. The captured snapshot binds additions, deletions and
absent resources; there is no input-list fallback when discovery fails.

## Failure and correction

An invalid registration and a failed assertion are different outcomes. In the
same temporary project, remove the subject without changing its registration:

```bash
set -euo pipefail
rm README.md
if cargo run --offline; then
  printf 'unexpected admission of the missing subject\n' >&2
  exit 1
fi
printf '# Example\n' > README.md
cargo run --offline
```

The missing discovered `resource:README.md` fails during definition
construction, before checker execution. Restore the native resource or correct
the registered subject reference, then rerun the same command. A rename also
needs a corrected subject reference; no trust flag or always-run override
repairs it.

To exercise a real assertion failure instead:

```bash
set -euo pipefail
printf 'Missing heading\n' > README.md
if cargo run --offline; then
  printf 'unexpected passing document check\n' >&2
  exit 1
fi
printf '# Example\n' > README.md
cargo run --offline
```

An attempted read of an existing unregistered analysis resource is a boundary
error even if the callable catches its returned error. Add the actual
responsible project subject to the definition, not a smaller JSON input list.
Changes after resolution require a new snapshot and registration. Tracked access
does not prove arbitrary Rust code avoids raw I/O: checks must use `Context`,
and provider conformance must still test deliberately omitted reads.
Observations from this example do not authorize reuse, skipped obligations or
publication.

## Inventory-wide admission

`Registry::resolve_query` is a target diagnostic: it resolves the requested
candidate definitions, but does not establish that every project duty is
covered. For inventory-wide deterministic execution use
`registry.admit(&snapshot, &inventory)?.run()?`. This path discovers the current
acceptance documents itself, resolves their exact checked definitions, and joins
all independently discovered obligations before returning an immutable `Plan`.
An unannotated registration cannot fill a coverage gap. A new resource or a
missing build/lint/test responsibility fails admission even when a document
provider resolves successfully. No caller-supplied target list defines coverage.

All sibling definitions are rechecked before the first execution. Each unit
retains every owning acceptance source when targets are shared across owners.
Observed results still require Evidence admission: accounting for responsibility
is not a proof that a check's body adequately tests its claimed behavior, and a
plan or a successful query is not publication authorization. Ordinary CLI gate
integration remains a separate unfinished consumer boundary; do not assume the
legacy input-list resolver uses these immutable plans.

## Supported engine-info effect

`provider::effect::Client::capture(executable, endpoint)` captures an explicit
absolute Podman client and a parsed absolute Unix `Endpoint`.
`Definition::container_engine_info` joins that client to current discovered
subjects, duties, and the producer resource. The constructor supplies the fixed
`podman --remote --url unix://… info --format json` operation; there is no
arbitrary command, argv, ambient connection, or SSH fallback. Query descriptions
include executable content identity, endpoint, platform, source origins,
required container/kernel capabilities, and exit-status mapping.

The owning annotation must use the explicit `[system]` lane; admission rejects
`[check]` or `[test]` relabeling of the effectful operation. Use this only for
an obligation genuinely requiring an engine observation, not to relabel a
deterministic check or satisfy unrelated semantic responsibilities. A missing
engine endpoint remains `unavailable`; nonzero engine exit remains `fail`;
malformed success output is an error, not a pass. Executable replacement
invalidates the checked definition. Resolution still checks the owning
annotation and current snapshot before producing a `Unit`. An engine-info pass
establishes only that observation, not host driver access, devices, lifecycle
authority, or full workspace readiness.

## Run the Nix API example

This example uses `Definition::hermetic` and the same generated query/resolution
path. It is also a diagnostic API example, not inventory-wide gate admission.
Run from the Loom checkout with provisioned Nix and immutable Bash. The native
project has no input registry: `src = self` stages the captured project, and
Bash's Nix context supplies its actual execution resource. Native discovery
retains the flake/system/attribute binding rather than splitting subject names
into commands. The constructor checks the recursive derivation graph and the
source/tool NAR reference closures.

```bash
set -euo pipefail
project=$(mktemp -d)
system=$(nix config show --json | jq -r '.system.value')
builder=$(readlink -f "$(command -v bash)")
builder_store="${builder%/bin/bash}"
mkdir -p "$project/specs/example" "$project/docs"
printf '# Example contract\n' > "$project/specs/example/spec.md"
printf '%s\n' '- [example](../specs/example/spec.md)' > "$project/docs/README.md"
printf '# Example\n' > "$project/README.md"
printf '# Example\n\n## Success Criteria\n\n- A document has a heading [check](document-heading)\n' > "$project/specs/example/tests.md"
cat > "$project/flake.nix" <<EOF
{
  outputs = { self }: {
    checks."$system".heading = builtins.derivation {
      name = "document-heading";
      system = "$system";
      builder = builtins.appendContext "$builder" {
        "$builder_store" = { path = true; };
      };
      src = self;
      args = [ "-c" "read -r heading < \"\$src/README.md\"; [[ \"\$heading\" == '# '* ]] && echo success > \"\$out\"" ];
    };
  };
}
EOF
git init "$project"
cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs
cargo run -p loom-gate --example nix-provider -- "$project"
```

The constructor resolves the discovered `heading` check uniquely and rejects a
missing or ambiguous native target. Descriptions bind the actual derivation,
staged source with executable-file identity, exact execution binary, graph
identity, offered document responsibility and source origins. Raw descriptions
and smaller input lists cannot replace that definition. The check must be a
deterministic, sandboxed derivation; `__noChroot`, host-network/impure-host
exceptions, impure/dynamic graphs, a network-enabled fixed-output check and
unregistered or traversing builder paths fail instead of becoming effectful
execution. This minimal constructor stages the complete project conservatively;
it does not provide fine-grained resource partitioning.

Execution forces sandboxing with no fallback, extra host paths, remote builders
or root-output substitution. Valid cold outputs build normally; already-realized
outputs are checked again rather than accepted as current evidence. An
unavailable sandbox yields an explicit `unavailable` outcome and a nonzero
example exit, not a pass or a suggestion to disable sandboxing. A different
native platform is `inapplicable`. Run on a worker with the declared capability
and platform to observe an actual check result.

### Native contract failure and correction

Removing the actual staged source makes the native execution contract invalid,
not a check with an empty input list:

```bash
set -euo pipefail
python3 - "$project/flake.nix" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace('      src = self;\n', ''))
PY
if cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs; then
  printf 'unexpected admission of omitted project source\n' >&2
  exit 1
fi
python3 - "$project/flake.nix" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace('      args = ', '      src = self;\n      args = '))
PY
cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs
cargo run -p loom-gate --example nix-provider -- "$project"
```

The construction error names the target and flake producer, carries the native
cause through normal stderr feedback, and does not dispatch the invalid graph.
Correcting the source re-enters the same query/admission path. Query success
alone does not mean the check ran, discharged baseline obligations or authorized
publication.
