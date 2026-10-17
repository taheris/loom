#!/usr/bin/env bash
set -euo pipefail

loom_bin="${1:?actual Loom binary is required}"
experiment="${2:?bounded experiment source is required}"
report="${3:?report output is required}"
report_name="${4:-artifact-storage-report.json}"
case "$report_name" in
    artifact-storage-report.json|rust-cache-context-report.json) ;;
    *) printf 'unsupported bounded fixture report: %s\n' "$report_name" >&2; exit 64 ;;
esac
fixture=$(mktemp -d)
cleanup() {
    local rc=$?
    if [[ "$rc" == 0 ]]; then
        rm -rf "$fixture"
    else
        printf 'failed gate fixture retained at %s\n' "$fixture" >&2
    fi
}
trap cleanup EXIT
install -m 0444 "$experiment" "$fixture/probe.py"
mkdir "$fixture/specs"
printf '# Storage experiment\n\n## Success Criteria\n\n- Private bounded measurements complete [check](python3 probe.py)\n' > "$fixture/specs/storage.md"
env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_COMMON_DIR \
    git -C "$fixture" init -q
"$loom_bin" --workspace "$fixture" gate check --tree
cp "$fixture/$report_name" "$report"
