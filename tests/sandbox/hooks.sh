#!/usr/bin/env bash
set -euo pipefail

source_root=$(realpath "${1:?usage: hooks.sh <source-root>}")
work=$(mktemp -d)
log="$work/hooks.log"

finish() {
    local status=$?
    if [[ "$status" -ne 0 && -f "$log" ]]; then
        printf 'sandbox hook verification failed:\n%s\n' "$(<"$log")" >&2
    fi
    rm -rf "$work"
    exit "$status"
}
trap finish EXIT

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

assert_payload_checked() {
    local payload="$1"
    if [[ ! -f .git/payload-checks ]] || ! grep -Fxq "$payload" .git/payload-checks; then
        fail "gate did not check payload: $payload"
    fi
}

expect_rejected_commit() {
    local hook_id="$1"
    local before
    before=$(git rev-parse HEAD)
    if git commit -m 'Reject invalid sandbox input' > "$log" 2>&1; then
        fail "ordinary commit bypassed $hook_id"
    fi
    grep -Fq -- "- hook id: $hook_id" "$log"
    [[ "$(git rev-parse HEAD)" == "$before" ]] || fail 'rejected commit changed HEAD'
}

test_commit_trailing_whitespace() {
    printf 'trailing space \n' > sanitation.txt
    git add sanitation.txt
    expect_rejected_commit trailing-whitespace
    [[ "$(<sanitation.txt)" == 'trailing space' ]] || fail 'sanitation did not repair whitespace'
    git reset --hard HEAD > /dev/null
}

test_commit_missing_final_newline() {
    printf 'missing newline' > sanitation.txt
    git add sanitation.txt
    expect_rejected_commit end-of-file-fixer
    [[ "$(wc -l < sanitation.txt)" -eq 1 ]] || fail 'sanitation did not add the final newline'
    git reset --hard HEAD > /dev/null
}

test_commit_merge_conflict() {
    printf '<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\n' > sanitation.txt
    git add sanitation.txt
    git rev-parse HEAD > .git/MERGE_HEAD
    printf 'Merge fixture\n' > .git/MERGE_MSG
    expect_rejected_commit check-merge-conflict
    git reset --hard HEAD > /dev/null
}

test_commit_formatter() {
    printf '{example=1;}\n' > flake.nix
    git add flake.nix
    expect_rejected_commit treefmt
    if git diff --quiet -- flake.nix; then
        fail 'treefmt did not apply its formatting repair'
    fi
    git reset --hard HEAD > /dev/null
}

test_commit_shell_reexec() {
    printf '#!/usr/bin/env bash\nset -euo pipefail\n' > reexec.sh
    printf "exec \"\$0\" \"\$@\"\n" >> reexec.sh
    git add reexec.sh
    expect_rejected_commit shell-reexec-explicit-interpreter
    grep -Fq 'self re-exec must name an explicit shell interpreter' "$log"
    git reset --hard HEAD > /dev/null
}

test_commit_integrity_gate() {
    printf '\n- Missing rubric [judge](./missing-rubric.sh#judge_missing)\n' >> specs/sandbox.md
    git add specs/sandbox.md
    expect_rejected_commit loom-gate-verify-files
    grep -Fq 'missing-rubric.sh' "$log"
    git reset --hard HEAD > /dev/null
}

test_commit_check_failure() {
    printf 'bad\n' > payload.txt
    git add payload.txt
    : > .git/payload-checks
    expect_rejected_commit loom-gate-verify-files
    assert_payload_checked bad
    grep -Fq 'sandbox payload rejected' "$log"
    git reset --hard HEAD > /dev/null
}

test_commit_accepts_repaired_input() {
    local before
    before=$(git rev-parse HEAD)
    printf 'updated\n' > payload.txt
    git add payload.txt
    : > .git/payload-checks
    git commit -m 'Accept repaired sandbox input' > "$log" 2>&1
    [[ "$(git rev-parse HEAD)" != "$before" ]] || fail 'valid commit did not advance HEAD'
    assert_payload_checked updated
    [[ -z "$(git status --porcelain)" ]] || fail 'valid commit left a dirty fixture'
    printf 'sandbox-commit-hooks-ok\n'
}

test_pre_push_runs_nix_and_gate() {
    local hook_id nix_check_output
    [[ ! -f .loom/marker.json ]] || fail 'fixture unexpectedly has a push marker'
    nix_check_output=$(nix eval --offline --raw ".#checks.$nix_system.fixture.outPath")
    [[ ! -e "$nix_check_output" ]] || fail 'fixture Nix check was already built'
    : > .git/payload-checks
    prek run --hook-stage pre-push --from-ref HEAD~1 --to-ref HEAD --verbose > "$log" 2>&1
    for hook_id in nix-flake-check full-test-suite loom-gate-verify-diff; do
        grep -Fq -- "- hook id: $hook_id" "$log"
    done
    grep -E '^nix flake check\.+Passed$' "$log"
    grep -E '^nix run \.#test-required \(full nextest and system coverage\)\.+Passed$' "$log"
    [[ -x "$nix_check_output" ]] || fail 'Nix flake check did not build fixture'
    grep -Eq '^[[:space:]]*sandbox-nix-required-ok$' "$log" || fail 'Nix required-test app did not run'
    assert_payload_checked updated
    printf 'bad\n' > payload.txt
    git add payload.txt
    : > .git/payload-checks
    if prek run --hook-stage pre-push --from-ref HEAD~1 --to-ref HEAD --verbose > "$log" 2>&1; then
        fail 'non-Nix pre-push gate accepted a failing verifier'
    fi
    grep -Fq -- '- hook id: loom-gate-verify-diff' "$log"
    assert_payload_checked bad
    grep -Fq 'sandbox payload rejected' "$log"
    printf 'sandbox-pre-push-hooks-ok\n'
}

# The fixture uses the packaged policy tools, never host Git state or skip flags.
while IFS= read -r variable; do
    unset "$variable"
done < <(git rev-parse --local-env-vars)
unset GIT_CONFIG_PARAMETERS SKIP PREK_SKIP PRE_COMMIT_ALLOW_NO_CONFIG
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export PREK_HOME="$work/prek" PREK_COLOR=never NO_COLOR=1
mkdir -p "$work/repo" "$work/home"
export HOME="$work/home"
cd "$work/repo"
git init -q
mkdir -p bin scripts specs checks
cp "$source_root/.pre-commit-config.yaml" .
cp "$source_root/bin/pre-push-checks" bin/
cp "$source_root/scripts/check-shell-reexec" scripts/
cp "$source_root/tests/sandbox/verify-payload.sh" checks/
chmod +x bin/pre-push-checks scripts/check-shell-reexec checks/verify-payload.sh
printf '.loom/\n' > .gitignore
nix_system=$(nix eval --offline --impure --raw --expr builtins.currentSystem)
nix_builder=$(realpath "$(command -v bash)")
nix_chmod=$(command -v chmod)
cat > flake.nix <<NIX
{
  outputs = { self }:
    let
      fixture = suffix: builtins.derivation {
        name = "sandbox-hook-nix-${work##*/}-\${suffix}";
        system = "$nix_system";
        builder = "$nix_builder";
        args = [ "-euc" ''
          printf '#!$nix_builder\\nprintf "sandbox-nix-required-ok\\\\n"\\n' > "\$out"
          $nix_chmod +x "\$out"
        '' ];
      };
    in {
      checks.$nix_system.fixture = fixture "check";
      apps.$nix_system.test-required = {
        type = "app";
        program = "\${fixture "app"}";
        meta.description = "Disposable sandbox hook fixture";
      };
    };
}
NIX
treefmt flake.nix >/dev/null
printf '# Sandbox fixture\n\n## Success Criteria\n\n- Accept valid payloads [check](./checks/verify-payload.sh)\n' > specs/sandbox.md
printf 'good\n' > payload.txt
printf 'clean\n' > sanitation.txt
printf '#!/usr/bin/env bash\nset -euo pipefail\nprintf "safe\\n"\n' > reexec.sh
git config user.name 'Sandbox verifier'
git config user.email 'sandbox@example.invalid'
git config commit.gpgsign false
: "${WRIX_PREK_HOOKS:?canonical Wrix hooks must be supplied}"
[[ -x "$WRIX_PREK_HOOKS/pre-commit" && -x "$WRIX_PREK_HOOKS/pre-push" ]] || fail 'packaged Wrix hooks are missing'
git config core.hooksPath "$WRIX_PREK_HOOKS"
git add .
git commit -m 'Seed sandbox hook fixture' > "$log" 2>&1
assert_payload_checked good

test_commit_trailing_whitespace
test_commit_missing_final_newline
test_commit_merge_conflict
test_commit_formatter
test_commit_shell_reexec
test_commit_integrity_gate
test_commit_check_failure
test_commit_accepts_repaired_input
test_pre_push_runs_nix_and_gate
