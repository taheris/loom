"""Exercise real cold Crane evaluation and Loom's data-only manifest builder."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

expression, manifest_path, native_path = map(Path, sys.argv[1:])
with tempfile.TemporaryDirectory(prefix="loom-eval-", dir=Path(os.environ["TMPDIR"]).resolve()) as temporary:
    command = [
        "nix", "eval", "--impure", "--offline", "--json", "--no-eval-cache",
        "--store", temporary,
        "--extra-experimental-features", "nix-command flakes",
        "--option", "allow-import-from-derivation", "false",
        "--file", str(expression),
    ]
    fixed = subprocess.run(command + ["fixed"], capture_output=True, text=True, timeout=120)
    assert fixed.returncode == 0, fixed.stderr
    result = json.loads(fixed.stdout)
    assert result["check"].endswith(".drv"), result
    assert result["metadata"].endswith("/Cargo.toml"), result
    assert result["lock"].endswith("/Cargo.lock"), result
    assert result["vendor"] and result["vendor"] != result["staged"], result
    assert not (Path(temporary) / result["staged"].lstrip("/")).exists(), "cold evaluation realized the staged source"
    legacy = subprocess.run(command + ["legacy"], capture_output=True, text=True, timeout=120)
    assert legacy.returncode != 0, "legacy implicit Cargo metadata unexpectedly succeeded cold"
    assert "allow-import-from-derivation" in legacy.stderr and "loom-src-with-extras" in legacy.stderr, legacy.stderr

manifest = json.loads(manifest_path.read_text())
native = json.loads(native_path.read_text())
for profile, runtimes in manifest.items():
    for runtime, entry in runtimes.items():
        assert set(entry) == {"ref", "source", "source_kind", "profile_config"}, entry
        assert Path(entry["source"]).is_file(), "image provenance lost its Nix context"
        config = json.loads(Path(entry["profile_config"]).read_text())
        assert config["agent"]["kind"] == runtime, config
        assert config["image"]["digest"] == "fixture-digest", config
        if profile == "rust":
            assert native[entry["profile_config"]] == "rust", native
print("Cold consumer check evaluation and data-only manifest provenance passed")
