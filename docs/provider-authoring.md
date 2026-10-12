# Checked provider API example

The tracked Rust API binds a named callable, discovered subjects, offered
duties, checker source/build identity and input access in one `Definition`.
`Registry` generates the typed query response and resolves it against the same
definition, current captured repository and independent `Inventory`. `Unit`
cannot be constructed from JSON or have its admitted fields rewritten.

This example is a diagnostic execution path, not a substitute for
`loom gate verify`, inventory-wide coverage, Evidence admission or publication.
The ordinary gate's provider integration and executable Nix/effect constructors
are not exposed by this example. Recognized Nix/effect candidate shapes alone do
not provide registrations or make an arbitrary command executable.

## Run the Rust example

From the Loom checkout, create a separate temporary native Cargo project. The
source below is copied from the shipped example, so the registered checker
source is also the code compiled for this project. Cargo supplies native
ownership and lockfile metadata; there is no second input table or hand-authored
description.

```bash
set -euo pipefail
loom_root="$PWD"
project=$(mktemp -d)
mkdir -p "$project/src" "$project/specs/example"
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
