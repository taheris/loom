# Actual Wrix Cargo projection and lifetime conformance

`wrix_cargo_storage_projection_matches_admitted_profile` is an explicitly
selected Rust/Nextest substrate test, not a native mount-planning substitute. It
requires Linux rootless Podman and the ticket-capable Rust Pi profile. It runs
no LLM/provider request and delivers a fresh empty fixture credential store.

```bash
bash scripts/configure-cargo-patches.sh
umask 077
LOOM_PROFILES_MANIFEST=/nix/store/...-profile-images.json \
  bash scripts/verify-wrix-cargo-projection
```

Alternatively select both `LOOM_CONFORMANCE_WRIX` and
`LOOM_CONFORMANCE_PROFILE`. The script resolves the manifest's Rust Pi pair when
explicit overrides are absent. It creates fresh private XDG state/cache/data/
configuration roots and uses Wrix's existing initialize/standalone-acknowledge
commands there. It never acknowledges or mutates an operator's live admission or
services. These roots and runtime evidence remain under `/tmp`.

The test selects the supplied production profile/image, disabling only unrelated
Nix-cache service injection in a fixture ProfileConfig copy. Production
namespace preparation, mount validation, wire serialization and host-only ticket
handoff are exercised. A fixed Pi startup extension runs after the production
entrypoint's firewall/capability drop, without an agent conversation. Declared
provider credentials are removed before launch; Podman inspection requests only
identity/mount fields, not raw environments.

Two launches exercise shared/private repository tool-cache policy. Shared policy
uses the configured common worker cache and both compiler wrappers; private
policy omits that mount. Guest Rust compilation, effective sccache backend/size,
a host/guest cache sentinel and actual container mounts establish routing.
Targets are writable and generation-private; integration, trusted-native,
operator and executing-runtime roots are excluded. Cache hits and foreground
completion are not evidence or disposal authority.

A third launch runs actual Cargo with a blocked build script. Killing the host
launcher and releasing its namespace lease leaves the container/compiler alive.
Pressure and a forged worker completion flag cannot delete its target; smaller
independent work remains admissible. After releasing the build script, Cargo
completion is only advisory: collection waits for the exact Wrix runtime
teardown and durable retirement, then removes only generated output. Source and
integration sentinels survive.

The owning system binding invokes this ignored test explicitly. Default-suite
skips never establish runtime proof. These are selected Linux profile results,
not Darwin/APFS sharing, arbitrary delegated compiler-service lifetime proof, or
a runtime disk quota. Native host cgroup conformance is separate:
`bash scripts/verify-native-cargo-lifetime`.
