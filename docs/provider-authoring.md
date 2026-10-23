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

## Native Cargo resources

Cargo target/module/include discovery supplies Rust membership. Conventional
`tests/fixtures` and `tests/snapshots` directories belong to the package's
native test targets. Other resource directories can be declared once in the
owning Cargo manifest, for example a trybuild suite:

```toml
[[package.metadata.loom.verify.resource]]
name = "cargo:example:resource:ui"
directory = "tests/ui"
role = "test-fixture"
consumers = ["cargo:example:test:public_contract"]
```

The directory is package-relative; membership follows current native snapshot
contents, including additions, removals and renames. Consumers must resolve to
actual discovered Cargo targets, and fixture consumers must be testable.
`generated` resources additionally require a discovered `producer` target and
nonempty consumers. `document` resources may contain only documentation. These
are directory definitions, not copied per-file policy inventories; declaring a
role does not discharge its responsibilities.

Workspace-owned shared resources use the same schema under
`[[workspace.metadata.loom.verify.resource]]`, relative to the workspace root,
and can reference discovered targets across its members. Spec-owned
`role_exception` declarations reference these resolved names, not file globs.
Missing directories, unknown consumers/producers and malformed resource metadata
fail discovery with the owning manifest as context.

`Inventory::checker_subjects` derives a checker source's native production
compilation subjects from its owning Cargo manifest and normal/build workspace
dependency edges. It retains library/binary/build-script subjects, manifests,
lock/configuration inputs and source includes, excluding unrelated integration
test/example/bench targets and fixture resources. Optional and target-specific
normal/build edges remain conservative; unit-test code within a library subject
is not split into a separate compilation subject. This API does not establish
that a provider has consumed the returned subjects.

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

## Loom-owned manifest producers

`loom_walk::walk::checked_definitions(&snapshot, &inventory, &targets)` returns
actual `Definition` values for the registered manifest dependency checks:
`loom_agent_deps`, `loom_llm_deps`, `loom_render_deps`, `loom_skill_deps`,
`loom_templates_deps`, `loom_tune_deps`, and `loom_events_is_leaf`. Pass those
values to `Registry::new`, then use its generated query and normal resolution
path. These callbacks read captured manifest text through `Context` and call the
same rule function as the standalone binary. Native target ownership supplies
the offered lint subjects; neither `LOOM_FILES` nor query JSON supplies them.

Dispatch and legacy input-query metadata now share one registry. Unknown,
repeated, and empty input queries fail rather than becoming a production-tree
fallback or a lossy batch. Metadata for an unconverted walk is still **not** a
checked registration: `checked_definitions` rejects it before constructing any
requested sibling. The ordinary CLI loads these checked definitions before scope
selection; unregistered legacy commands cannot bridge into execution. This
producer API alone does not establish inventory-wide coverage or authorize
publication.

## Inventory-wide admission

`Registry::resolve_query` is a target diagnostic: it resolves the requested
candidate definitions, but does not establish that every project duty is
covered. `Registry::resolve_owned` joins all current acceptance owners into one
unit per exact diagnostic target. An empty owner set or any invalid owner
rejects the complete request before any unit is exposed. For inventory-wide
deterministic execution use `registry.admit(&snapshot, &inventory)?.run()?`.
This path discovers the current acceptance documents itself, resolves their
exact checked definitions, and joins all independently discovered obligations
before returning an immutable `Plan`. An arbitrary unannotated registration
cannot fill a coverage gap. The checked `hermetic_baseline` constructor supplies
only independent native duties and Nix/configuration resources actually captured
in its runtime or evaluator partition; it cannot claim unused sources, other
outputs or document assertions. A new resource or a missing build/lint/test
responsibility fails admission even when a document provider resolves
successfully. No caller-supplied target list defines coverage.

The fixed library-owned `loom:project-context` reader supplies the ordinary
text/document baseline. Its serialized Rust checker identifies the compiled
library policy, source digest and running artifact, not a pretend Rust source
file in the consumer repository. It reads captured document text, rejects NUL,
parses Markdown, and checks known Loom TOML and pre-commit YAML syntax. Known
Git scope configuration and spec-relative judge rubric files remain captured
data; rubric collection here executes no script. Native fixtures/generated
resources retain their consumer/producer duties and are not reclassified as
documents. This reader offers no Rust build/lint/test duties, semantic
correctness assertion, or coverage for otherwise unknown files. Candidate JSON
cannot register library callbacks or alter their source identity.

Shared Cargo suites use
`checked.cargo { craneLib; operation; resources; evaluation; args; }` in
`nix/verification.nix`. Its closed operations are `build`, `lint`,
`lint-all-targets`, and `test`; the constructor selects the fixed offline/locked
workspace command, not caller shell wiring. Build and test cover all targets,
while production and all-target lint remain separate units. Only constructor
arguments for sources, dependency artifacts, pinned tools and fixture
environment are accepted; phase overrides are rejected. Admission also compares
the actual derivation phase and standard builder entry scripts with the declared
operation before offering Rust build/lint/test duties, rejects direct
build-command and shell-startup overrides, and requires each subject's runtime
source closure. A Cargo label on an unrelated derivation is rejected. Runtime
helper resources receive mandatory duties through their validated native
consumers; this does not turn unconsumed files into implicit fixtures or erase
existing Rust roles.

A system report may request `runtime = "native-cgroup"` when it needs real
native process-generation control. This is a closed isolated execution profile,
not a host-command override. The constructor still binds immutable programs,
sources, the complete report contract and supervisor. Compiler/input recipes
remain offline sandboxed Nix realizations. The existing trusted driver launches
the producer in a bounded transient user service with private PID/network/tmp
namespaces, masked home and runtime sockets, a read-only system, and only its
own delegated cgroup writable. Private owned `/sys` ancestors surround the
actual kernel delegation; native ownership and filesystem guards are not
relaxed. Worker contexts cannot dispatch this profile. Missing
delegation/tooling is a non-pass, never publication credit. Each invocation has
a fresh supervised report and retains its actual transcript; query JSON cannot
select an executable, change the profile or grant capabilities.

For scoped API execution, resolve the existing `scope::Request` against the
workspace, then call `plan.run_scoped(&scope)`. It uses the ordinary scope
selector with `InputResolver::from_plan(&plan)`: inputs come from immutable
admitted units, including test-tier definitions and all shared acceptance
owners. Raw input-query helpers and Cargo scope guesses do not replace those
inputs. An empty finite scope runs no units; exact-target selection remains a
diagnostic, not whole-project success. Inventory-wide admission still precedes
selection, so narrowing the scope cannot hide a missing obligation.

All sibling definitions are rechecked before the first execution, even when a
scope selects only one target. Constructing an input projection runs neither
queries nor checkers and rejects a stale plan; dispatch rechecks freshness
again. Each unit retains every owning acceptance source when targets are shared
across owners. Observed results still require Evidence admission: accounting for
responsibility is not a proof that a check's body adequately tests its claimed
behavior, and a plan or a successful query is not publication authorization.
`plan.select_at(&scope, tiers, Stage::Feedback)` retains selected and excluded
siblings with explicit reasons and full analysis inputs. Criterion-level
`defer_until = "publication"` affects finite feedback selection only; whole-tree
and publication requests retain it. An eligible sibling sharing the same
execution is not deferred. Only parametric property campaigns explicitly marked
`affected` may be sliced; default and nonparametric groups keep their complete
native collection. Empty affected groups are valid zero-dispatch selections, not
invalid registrations.

`selection.explain()` serializes scope, stage, owning criteria, baseline duties,
input origins and fresh/shared execution decisions without invoking verifiers.
`selection.run_report()` records actual durations, per-owner observations and
fresh/shared invocation counts. Sharing is confined to equivalent executions in
one current run, including separately constructed identical native graphs;
endpoint, executable and complete execution inputs must still agree. Whole-plan
`run()` and mint use the same sharing path. No prior result is supplied to this
API, and these reports cannot be deserialized into a plan or admitted evidence.

The ordinary deterministic CLI now collects checked definitions before invoking
project hooks or verifiers, consumes admitted selection (including unannotated
native baseline units), retains eligible owners and reports its plan. Exact
target requests remain diagnostics. Unknown definitions, unsupported native app
effects and unresolved independent duties fail closed; no configured command or
raw input descriptor is a fallback. This is not a claim that every existing
repository annotation is already registered, or that downstream drivers have
adopted the new provider interface.

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

### Run the supported effect example

This tests the explicit engine connection configuration's consuming test
obligation, not build/lint coverage or general worker readiness. Use only an
engine endpoint already authorized in the calling environment. Do not add engine
socket mounts or host capabilities to ordinary workers to make this example
pass. Set `engine_client` and `engine_endpoint` explicitly before running:

```bash
set -euo pipefail
: "${engine_client:?absolute permitted podman client is required}"
: "${engine_endpoint:?absolute permitted Unix endpoint is required}"
loom_root="$PWD"
bash "$loom_root/scripts/configure-cargo-patches.sh"
project=$(mktemp -d)
mkdir -p "$project/src" "$project/.cargo" "$project/specs/example" "$project/docs"
cp "$loom_root/crates/loom-gate/examples/effect-provider.rs" "$project/src/main.rs"
cp "$(readlink -f "$loom_root/.cargo/nix-patches.toml")" "$project/.cargo/config.toml"
printf '/target/\n' > "$project/.gitignore"
printf '# Engine observation contract\n' > "$project/specs/example/spec.md"
printf '%s\n' '- [example](../specs/example/spec.md)' > "$project/docs/README.md"
printf '## Success Criteria\n\n- The explicit engine connection supports info [system](engine-info)\n' > "$project/specs/example/tests.md"
printf 'executable = "%s"\nendpoint = "%s"\n' "$engine_client" "$engine_endpoint" > "$project/engine.toml"
printf '[package]\nname = "engine-effect-example"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\nloom-gate = { path = "%s/crates/loom-gate" }\nanyhow = "1"\nserde = { version = "1", features = ["derive"] }\nserde_json = "1"\ntoml = "1.1"\n' "$loom_root" > "$project/Cargo.toml"
git init "$project"
cd "$project"
cargo generate-lockfile --offline
cargo run --offline -- --print-inputs
cargo run --offline
```

A successful invocation records only the actual fixed engine-info observation.
The same candidate/resolution path retains `unavailable` for an absent endpoint;
restoring the configured connection and rerunning requires a fresh current
snapshot and definition, not a copied successful result.

## Run the Nix API example

This example uses `Definition::hermetic` and the same generated query/resolution
path. It is also a diagnostic API example, not inventory-wide gate admission.
Run from the Loom checkout with provisioned Nix and immutable Bash. The native
project uses `nix/verification.nix`: resource values retain origins while
constructing filtered native sources, and the check builds from those same
values. Bash's Nix context supplies its actual execution resource. Discovery
retains the namespace/flake/system/attribute binding instead of splitting
subject names into commands. Construction checks the recursive derivation graph
and source/tool NAR closures, then reevaluates in the exact source partition.
Derivation documents passed as readonly source data remain in that complete
native description, but only actual dependency edges enter the executable safety
check and realization frontier. Reading a recipe never authorizes it.

Constructor-provided origins follow actual transitive derivation dependencies.
An unrelated output cannot borrow another constructor's source membership.
Wrapping an already constructed result retains its resource and evaluator
origins. Discovery batches immutable graph facts and shares node storage, never
outcomes; partition sharing requires the complete effective source and evaluator
scope. `tests/verify/native-resources.py` independently traverses the native
dependency closure and checks every shipped current-system partition.

Native check, package and app definitions retain evaluation/check obligations
independently of constructor registration. The app constructor checks that its
immutable program artifact exists and is executable; it never runs the app or
its arguments. Foreign-system obligations remain explicitly inapplicable rather
than disappearing or becoming passing executions. A fixed read-only
native-platform classifier records actual backend configuration without
constructing foreign recipes or fabricating an executable derivation. Changes to
its pinned tool or platform context require fresh construction. Wholly foreign
programs retain unverified source duties; an applicable consumer always needs
its own checked construction. Native producers run their registered analysis or
test program directly: they must not recursively admit the whole project graph
containing themselves.

```bash
set -euo pipefail
project=$(mktemp -d)
system=$(nix config show --json | jq -r '.system.value')
builder=$(readlink -f "$(command -v bash)")
builder_store="${builder%/bin/bash}"
cp nix/verification.nix "$project/verification.nix"
mkdir -p "$project/specs/example" "$project/docs"
printf '# Example contract\n' > "$project/specs/example/spec.md"
printf '%s\n' '- [example](../specs/example/spec.md)' > "$project/docs/README.md"
printf '# Example\n' > "$project/README.md"
printf '# Example\n\n## Success Criteria\n\n- A document has a heading [check](document-heading)\n' > "$project/specs/example/tests.md"
cat > "$project/flake.nix" <<EOF
{
  outputs = { self }: let
    checked = import ./verification.nix;
    source = checked.resource {
      root = ./.;
      filter = path: type: type == "directory" || path == toString (./. + "/README.md");
    };
  in {
    checks."$system".heading = checked.check {
      resources = [ source ];
      build = sources: builtins.derivation {
      name = "document-heading";
      system = "$system";
      builder = builtins.appendContext "$builder" {
        "$builder_store" = { path = true; };
      };
      src = builtins.head sources;
      args = [ "-c" "read -r heading < \"\$src/README.md\"; [[ \"\$heading\" == '# '* ]] && echo success > \"\$out\"" ];
      };
    };
  };
}
EOF
git init "$project"
cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs
cargo run -p loom-gate --example nix-provider -- "$project"
```

Native inventory enumerates every declared system and check, package, and app
name from a locked flake reference without evaluating individual output values.
Each system namespace must still evaluate to an attribute set, including foreign
systems. This discovers obligations, not executable definitions: an applicable
thrown or non-derivation output still fails ordinary checked construction. A
malformed system namespace fails discovery rather than becoming an empty check
set. No platform is dropped or converted to a passing result by discovery.
Foreign applicability requires the separate actual-backend observation described
above.

The constructor resolves the discovered `heading` check uniquely and rejects a
missing or ambiguous native target. Descriptions bind the actual derivation,
staged source with executable-file identity, exact execution binary, graph
identity, offered document responsibility and source origins. Raw descriptions
and smaller input lists cannot replace that definition. The check must be a
deterministic, sandboxed derivation; `__noChroot`, host-network/impure-host
exceptions, impure/dynamic graphs, a network-enabled fixed-output check and
unregistered or traversing builder paths fail instead of becoming effectful
execution. An app is a host program, not a derivation: discovery cannot turn its
program string into a hermetic bridge. Packages resolve through their actual
namespace, not a fabricated check alias.

Only actual native source files and evaluator resources enter the isolated
partition, along with Nix definitions and lockfiles. Every retained origin must
match captured bytes and executable mode. Omitted reads fail reevaluation;
existence-dependent changes fail graph equality. `evaluation` resources support
module data such as patches without pretending those resources are runtime
fixtures. Missing responsibility still fails even when a smaller graph would be
hermetic. `src = self` whole-project runtime captures are rejected.

A standalone Rust file supplied to a foreign Nix build must be declared as an
exact file resource and appear in that build's validated execution closure. That
input retains its Nix consumer's mandatory obligations. Evaluator-only or
directory-wide captures cannot claim orphan Rust files, and declaring a Cargo
source as a Nix input never removes its Cargo Build, Lint or Test obligations.
These input facts cannot authorize named Rust tests instead of compiler facts.

The shipped workspace exports `verification-build`, `verification-clippy`,
`verification-clippy-all-targets`, and `verification-nextest` packages.
Production and all-target lint remain separate, sharing Cargo dependency
artifacts and vendoring. Extra test sources are constructed individually, not
interpolated through a whole-repository store reference. These packages are
deliberately not added to the fast `checks` tier.

The fixed compiler-only catalogue companion may reuse its immutable build
output; its source/phase identity and compiler inventory are still validated.
That inventory is not a test verdict. Actual test/report roots still execute
freshly. Within one pure admission pass, aliases share actual graph/resource
reads, not results. The snapshot, tools and every distinct graph are rechecked
before effects. Backend-supported extra platforms use the same batched
construction; unsupported platforms retain their separate applicability
obligations.

`python3 tests/verify/native-resources.py .` reevaluates all four actual
workspace derivations from their exact partitions with IFD disabled. The
toolchain still uses the existing manifest URL/hash pin, read directly rather
than through an evaluator-executed derivation.

The default Nix profile forces sandboxing with no fallback, extra host paths,
remote builders or root-output substitution. Valid cold outputs build normally;
already-realized outputs are checked again rather than accepted as current
evidence. An unavailable sandbox yields an explicit `unavailable` outcome and a
nonzero example exit, not a pass or a suggestion to disable sandboxing. A
different native platform is `inapplicable`, not passing coverage. The fixed
automated acceptance app `nix run .#test-native-units`, called by the
required-suite app, first verifies actual workspace resource partitions and then
runs the fixed `native-isolation` executable. It exercises cold outputs, forced
warm rebuilding, all output mappings, an undeclared host file and a live
disposable loopback fixture inaccessible from the build's network namespace.
Host controls prove the file exists and the listener is reachable in both
phases. Missing sandbox capability exits78, never passing or silently skipping.
Required capability gaps remain explicit infrastructure failures. VM routing has
been removed at the user's request: neither the suite nor the native acceptance
app boots a fallback machine. An existing capable native executor is required
for those checks; this does not authorize a manual-test completion claim,
exposing sockets, disabling sandboxing or expanding worker capabilities. Cold
root-output coverage does not claim a fully empty store or foreign-platform
execution.

### Supervised native reports

`checked.cargo` retains fresh Nextest `libtest-json-plus` reports separately
from its public Nix assertion. The assertion still fails when the producer
fails. Native execution binds both derivations, checks their exact relationship,
and realizes a fresh reporting derivation without substituting a previous
verdict. Only the invocation nonce and its resulting output path may differ;
execution arguments, resources, dependencies and prerequisite checks remain
bound to the admitted graph.

Compiler-authorized case aliases project their individual terminal results.
Sharing an invocation retains its complete report: a passing case cannot replace
a failing independent suite baseline. Missing or unexpected identities,
contradictory producer status and interrupted producers cannot create passing
coverage. The ordinary CLI persists retained verdict metadata rather than
reconstructing a result from the suite's exit code. Producer JSON never supplies
skip permission.

For opaque Check/System producers, `checked.reported` accepts `pkgs`, `name`,
`resources`, `evaluation`, an immutable `program` and `arguments`, a parser
(`json-lines`, `junit-xml` or `libtest-json`), and complete result `targets`.
Annotations address these results as
`nix report .#checks.<system>.<attribute>::<result>` (or the actual package
namespace). These addresses resolve constructor-owned data; they are never shell
commands. They cannot authorize Rust Test identities in place of compiler facts.
The separate public assertion checks producer status while the native provider
also validates report completeness and consistency. Unpermitted skips remain
unverified; report retention does not waive independent baseline obligations.

### Native contract failure and correction

Removing the actual staged source makes the native execution contract invalid,
not a check with an empty input list:

```bash
set -euo pipefail
python3 - "$project/flake.nix" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace('      src = builtins.head sources;\n', ''))
PY
if cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs; then
  printf 'unexpected admission of omitted project source\n' >&2
  exit 1
fi
python3 - "$project/flake.nix" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace('      args = ', '      src = builtins.head sources;\n      args = '))
PY
cargo run -p loom-gate --example nix-provider -- "$project" --print-inputs
cargo run -p loom-gate --example nix-provider -- "$project"
```

The construction error names the target and flake producer, carries the native
cause through normal stderr feedback, and does not dispatch the invalid graph.
Correcting the source re-enters the same query/admission path. Query success
alone does not mean the check ran, discharged baseline obligations or authorized
publication.
