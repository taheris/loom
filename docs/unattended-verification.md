# Unattended verification preparation

Verification must not wait for a person to approve downloads. Acquire the
immutable inputs before dispatch; run worker admission and verification offline.
Preparation is availability, not a passing test or an isolation verdict.

## Trusted provisioning phase

From the selected checkout and its pinned inputs, run:

```sh
nix run .#prepare-verification -- prepare
```

This realizes the `verification-sdk` output, including the workspace's
`preparationInputs` closure and formatter. That closure retains the compiler,
Cargo vendor/dependency artifacts, actual native tools and pinned SDK sources.
It excludes the staged test-source graph; fixtures remain cheap offline recipes
using these tools. It does not enumerate every exported package, build unrelated
optional backends, or execute the workspace test suite. Unlike fetching
fixed-output source archives alone, it materializes the executable dependency
artifacts.

The command retains a Nix GC root under
`$XDG_STATE_HOME/loom/verification/<sdk-store-name>/sdk` (defaulting to
`$HOME/.local/state`). A changed SDK identity gets a separate root; preparation
does not remove old roots or perform store-wide collection. State ancestors must
be owned, non-indirect directories without unsafe write permissions.

Acquisition has a twenty-minute limit, bounded connection/stall timeouts, and a
retained `provision.log`. Failure blocks dispatch and reports the log location;
it does not start an interactive approval loop or retry from a test hook.

## Image and startup integration

Loom's Rust profile factory and standalone Rust worker images include the SDK as
a concrete package, so image realization acquires it before the worker starts.
Factory-managed Rust agents run the immutable preparation executable in `check`
mode before the existing native tool preflight. This mode uses only local store
validity queries, never a build, flake evaluation, substitution or network
acquisition. It reports all missing paths from the retained manifest in bounded
batches and has a 65-second overall limit. A missing SDK manifest reports its
immutable identity and requires preparation before dispatch.

For a reusable prepared store outside those images, retain the preparation
executable too and invoke its immutable path with `check` before dispatch.
`nix --offline run .#prepare-verification -- check` is also available when the
flake inputs and executable are already local. Do not use an online `nix run` as
the worker's readiness check: evaluation itself can acquire flake inputs.

Consumers calling `mkProfileManifest` directly can pass `verificationSdk` to
retain their selected closure and enable the same startup check. The argument is
optional for compatibility; omitting it does not establish SDK readiness. A
lock/toolchain/dependency change requires preparing the new selected SDK or
rebuilding the worker image. Existing test hooks remain offline and fail closed
if newly declared prerequisites are unavailable.

## Temporary lean Wrix image planner

The Wrix package sets temporarily override `dockerMakeLayers` using
`nix/wrix-layer-overlay.nix`. The adapter supports only Wrix's
`remove_paths → popularity_contest → limit_layers` pipeline. It preserves the
pinned upstream popularity recurrence, name tie-breaking, exclusions and tier
budgets without importing python-igraph or its optional plotting/dataframe test
dependencies. The normal Docker/OCI builders, image contents, runtime selection
and upstream package tests are unchanged. Unsupported pipelines and cyclic or
incomplete reference graphs fail explicitly rather than falling back.

`checks.<system>.wrix-lean-layer-plan` compares the adapter with unmodified pure
functions extracted from the pinned Nixpkgs planner across generated DAGs,
exclusions and budgets. It also checks path retention, deterministic ordering,
shared dependencies, CLI operation and invalid inputs. Remove this scoped
overlay and adapter once the Wrix pin provides a tested lean planner. This is
not an assertion that image/backend acceptance has passed.

## Persistent network approval belongs to the driver

The trusted image/store provisioner needs a persistent, phase-scoped firewall
policy for its configured Nix binary cache (normally `cache.nixos.org`) and any
explicitly approved redirects or pinned source origins needed on cache misses.
Prefer a prebuilt SDK in the driver's trusted cache to approving an open-ended
set of upstream source hosts. Preserve Nix signature/hash checking. Do not
accept redirects as implicit approval for arbitrary hosts.

Apply that policy to provisioning, not to test subprocesses or the worker's
ordinary execution authority. Provision the selected image/store at worker
creation and reuse it across sessions. If acquisition is denied, report an
infrastructure blocker before dispatch rather than asking the absent user to
approve a test run. This repository does not edit the driver's firewall or host
configuration: deployment of the prepared image and persistent provisioning
approval remains a trusted driver/operator action. Already-running images are
not silently replaced.

## Kernel-free landing and explicit diagnostics

Ordinary Loom packages, checks and app constructors use `native-process`
verification inside the existing agent boundary. `nix run .#smoke` exercises
Loom's real CLI/mock-agent launch and completion seams without starting another
container. Cargo-retention acceptance runs real portable process and storage
cases; unavailable lifecycle authority must retain output conservatively.
Neither producer needs namespaces, mounts, a delegated cgroup or a systemd user
service. CLI smoke executes its selected cases from a reusable immutable Nextest
archive rather than compiling dependencies in every private runtime fixture.
That archive captures Cargo/code, test configuration and Git fixture inputs, not
unrelated documentation or Nix declarations. Every smoke invocation still
executes the real cases; archive availability is never test evidence. The
native-surface check evaluates every ordinary exported definition; the landing
check executes signed fixture commits and configured commit/pre-push hooks,
including failure controls. Run those checks under namespace denial when
validating an agent image.

`nix run .#test` and the required pre-push suite do not invoke the explicit
`nix run .#test-native-units` kernel/containment qualification app. Ordinary
provider semantics remain exercised by the complete native suite and reports.

Container substrate diagnostics remain available explicitly as
`nix run .#smoke-container`. Their heavy runtime lives under
`legacyPackages.<system>.kernel-smoke-runtime`, outside the ordinary landing
catalogue, and is acquired only on that explicit request. The two actual cgroup
qualification cases remain separately invokable on a capable host:

```sh
out="$(mktemp -d)" nix run .#legacyPackages.aarch64-linux.kernel-cargo-retention -- --kernel
```

These commands may require kernel capabilities. They are diagnostics, not an
implicit publication prerequisite or a passing result when unavailable. The
normal retention report never invokes the two kernel cases or claims their
coverage. Wrix still owns backend/container qualification; generic Gate support
for consumers explicitly declaring effectful verifiers remains fail-closed.

The SDK and its readiness check are not test evidence. Native-process results
remain distinct from sandbox/isolation acceptance, and no configured hook,
signing step or test case is bypassed by preparation.
