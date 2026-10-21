# Actual Wrix Cargo projection conformance

`wrix_cargo_storage_projection_matches_admitted_profile` is an explicitly
selected Rust/Nextest substrate test, not a native mount-planning substitute. It
requires Linux rootless Podman and a Rust Pi profile. It runs no LLM or provider
request and delivers only a fresh empty fixture credential store.

```bash
bash scripts/configure-cargo-patches.sh
umask 077
LOOM_CONFORMANCE_WRIX=/absolute/path/to/wrix \
LOOM_CONFORMANCE_PROFILE=/nix/store/...-wrix-rust-pi-profile-config.json \
bash scripts/verify-wrix-cargo-projection
```

The system binding invokes the ignored native test explicitly; a default-suite
skip is never accepted as this runtime proof. The wrapper can resolve the
selected Rust Pi launcher/profile from `LOOM_PROFILES_MANIFEST` when explicit
fixture overrides are absent.

The test retains its temporary directory under the host's `/tmp` and prints its
location, independently of an enclosing disposable native-fixture runner. It
selects the supplied production profile/image, disabling only unrelated
Nix-cache service injection in a fixture copy of ProfileConfig. It never updates
or stops existing services. All generated worker targets come from production
Cargo namespace preparation and dispatch; Loom's production Wrix wire serializer
encodes the launch. An extension executes a fixed shell probe at Pi startup,
after the production entrypoint's firewall and capability drop, without an
agent/model conversation.

Two actual launches exercise inherited shared and private repository tool-cache
policy. The shared launch receives Loom's configured worker cache mount and both
compiler-wrapper settings. The private launch omits it. Guest compilation,
sccache's effective backend/max-size report, a host/guest cache sentinel and
actual Podman container mount inspection jointly establish routing, rather than
inferring it from environment names. The test checks exact target source,
destination, writable guest output and exclusion of integration, trusted-native
and running-executable roots. It preserves dispatched targets after completion;
no result is lifecycle disposal authority.

## Linux result

Executed successfully on the Linux host with Podman 5.8.9, the pinned Wrix host
launcher `0ndszzhsliqajrybivc4ld200dn1wi1y-wrix`, and Rust Pi profile
`wi00q4j7x8dxdxsks7z6dj37xsi46rxs-wrix-rust-pi-profile-config.json`. The
selected image is `localhost/wrix-rust-pi:de1a24f5`, digest
`sha256:33dfac329eda022ba449eabf86c121bae7e50d45fc6d8ce176469082af5e2053`. Both
launches passed. Retained runtime inspection, backend reports, launch configs
and logs are under `/tmp/.tmpEQ2qZd` on that host.

This establishes that selected Linux Rust profile's mount/backend substrate. It
does not establish newer Wrix shared-compiler-service routing, Darwin/APFS
physical sharing, process lifetime reconciliation, completed-output collection,
or bounded runtime growth. Those are separate acceptance boundaries.
