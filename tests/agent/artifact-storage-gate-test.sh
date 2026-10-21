#!/usr/bin/env bash
set -euo pipefail

loom_bin="${1:?actual Loom binary is required}"
gate="${2:?storage gate script is required}"
runner="${3:?private fixture runner is required}"
fixture=$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/loom-storage-root-test.XXXXXXXX")
trap 'rm -rf -- "$fixture"' EXIT
mkdir -m 0700 "$fixture/builder" "$fixture/temporary"
mkdir -m 0777 "$fixture/unsafe"
ln -s "$fixture/temporary" "$fixture/alias"
bash "$runner" python3 - <<'PY'
import json
import os
from pathlib import Path
import stat

state = Path(os.environ["XDG_STATE_HOME"])
assert state == Path(os.environ["TMPDIR"]) / "state"
policy = state / "loom/trusted-cargo/policy.json"
assert stat.S_IMODE(policy.stat().st_mode) == 0o600
assert policy.stat().st_nlink == 1
assert json.loads(policy.read_text()) == {"reservation_bytes": 1048576}
PY
cat > "$fixture/probe.py" <<'PY'
import json
import os
from pathlib import Path
import stat

runtime = Path.cwd().parent
assert runtime.parent.parent == Path(os.environ["EXPECTED_ROOT"]), runtime
assert stat.S_IMODE(runtime.stat().st_mode) == 0o700
assert Path(os.environ["TMPDIR"]) == runtime
assert Path(os.environ["XDG_STATE_HOME"]) == runtime / "host-state"
Path("artifact-storage-report.json").write_text(json.dumps({"runtime": str(runtime)}))
print(json.dumps({"pass": True, "evidence": "private temporary root is inherited"}))
PY

(
    umask 000
    EXPECTED_ROOT="$fixture/builder" NIX_BUILD_TOP="$fixture/builder" TMPDIR="$fixture/unsafe" \
        bash "$runner" bash "$gate" "$loom_bin" "$fixture/probe.py" "$fixture/builder-report.json"
    EXPECTED_ROOT="$fixture/temporary" TMPDIR="$fixture/alias" \
        env -u NIX_BUILD_TOP bash "$runner" bash "$gate" "$loom_bin" "$fixture/probe.py" "$fixture/temporary-report.json"
)
python3 - "$fixture/builder-report.json" "$fixture/temporary-report.json" <<'PY'
import json
from pathlib import Path
import sys

for report in sys.argv[1:]:
    assert not Path(json.loads(Path(report).read_text())["runtime"]).exists()
PY
