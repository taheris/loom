# modules/flake/overlays.nix
#
# Pins the upstream agent-protocol versions loom is tested against.
# Bumps are deliberate PRs accompanied by the protocol-bump checklist
# (re-run parser tests; scan upstream changelog for new event types;
# add `Unknown` coverage if any new types lack typed variants; update
# mock scripts if new types reach pipe-level paths). No live wire
# tests run against real binaries; silent breaks in exercised fields
# surface as `serde_json` errors in parser tests on bump.
#
# Spec: specs/tests.md § Cross-platform + CI integration /
#       Non-Functional #9 (Upstream protocol versioning).
_:

let
  # The pinned wire-protocol versions parser tests + mock scripts target.
  # Bump in lockstep with the protocol-bump checklist above.
  protocolVersions = {
    # pi-mono — pi RPC protocol producer.
    pi-mono = "0.72.0";
    # Claude Code — stream-json producer.
    claude-code = "2.0.34";
  };

  # Overlay that publishes the pinned versions onto the `pkgs` set
  # under `loomProtocolVersions`, so per-system modules can grep them
  # at evaluation time. The overlay is intentionally inert otherwise
  # — pi-mono and Claude Code are not yet packaged for the sandbox
  # image; the constants alone are the contract surface.
  overlay = _final: _prev: {
    loomProtocolVersions = protocolVersions;
  };
in
{
  flake.overlays.default = overlay;
}
