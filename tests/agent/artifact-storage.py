#!/usr/bin/env python3
"""Bounded private Cargo storage experiment; never collect real workspace outputs."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time


LIMIT = 256 * 1024 * 1024


def run(argv, cwd, env, expected=0):
    started = time.monotonic()
    result = subprocess.run(
        argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=120
    )
    assert result.returncode == expected, (argv, result.returncode, result.stderr)
    return result, round(time.monotonic() - started, 4)


def usage(root):
    seen = set()
    allocated = apparent = files = 0
    if root.exists():
        for path in root.rglob("*"):
            info = path.lstat()
            if stat.S_ISREG(info.st_mode):
                apparent += info.st_size
                files += 1
                key = (info.st_dev, info.st_ino)
                if key not in seen:
                    allocated += info.st_blocks * 512
                    seen.add(key)
    assert allocated <= LIMIT, (root, allocated, LIMIT)
    return {"allocated": allocated, "apparent": apparent, "files": files}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_state(project, env):
    state = {}
    for name, args in {
        "head": ["rev-parse", "HEAD"],
        "stashes": ["stash", "list", "--format=%H"],
        "status": ["status", "--porcelain=v1"],
        "diff": ["diff", "--binary", "HEAD"],
    }.items():
        state[name] = run(["git", *args], project, env)[0].stdout
    state["notes"] = digest(project / "notes.txt")
    return state


def experiment(root):
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("GIT_", "SCCACHE_"))
    }
    env.update(
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_SYSTEM="/dev/null",
        CARGO_HOME=str(root / "cargo-home"),
        CARGO_INCREMENTAL="0",
        CARGO_BUILD_JOBS="1",
        RUSTFLAGS="-C debuginfo=0",
        HOME=str(root / "home"),
    )
    for key in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
    ]:
        env.pop(key, None)
    (root / "home").mkdir(mode=0o700)
    a, b = root / "a", root / "b"
    a.mkdir()
    (a / "src").mkdir()
    (a / "Cargo.toml").write_text(
        '[package]\nname="artifact-storage-fixture"\nversion="0.0.0"\nedition="2024"\n'
        "[workspace]\n[features]\nvariant=[]\n"
    )
    library = 'pub fn value() -> &\'static str { "one" }\n'
    (a / "src/lib.rs").write_text(library)
    (a / "src/main.rs").write_text(
        'fn main() { println!("{}", artifact_storage_fixture::value()); }\n'
    )
    (a / ".gitignore").write_text("/target/\n")
    (a / "notes.txt").write_text("preserved source notes\n")
    run(["git", "init", "-q"], a, env)
    run(["git", "config", "user.name", "Storage fixture"], a, env)
    run(["git", "config", "user.email", "fixture@example.invalid"], a, env)
    run(["git", "add", "."], a, env)
    run(
        ["git", "-c", "commit.gpgsign=false", "commit", "-qm", "Synthetic fixture"],
        a,
        env,
    )
    run(["git", "clone", "--quiet", "--no-hardlinks", str(a), str(b)], root, env)
    targets = root / "targets"
    targets.mkdir(mode=0o700)
    report = {"rustc": run(["rustc", "--version", "--verbose"], a, env)[0].stdout}
    binary_name = "artifact-storage-fixture"

    def build(project, target, extra=(), override=None):
        target.mkdir(mode=0o700, exist_ok=True)
        assert stat.S_IMODE(target.stat().st_mode) == 0o700
        build_env = env | {"CARGO_TARGET_DIR": str(target)} | (override or {})
        result, elapsed = run(
            ["cargo", "build", "--offline", *extra], project, build_env
        )
        usage(targets)
        executable = target / "debug" / binary_name
        observed = run([str(executable)], project, env)[0].stdout.strip()
        return {
            "seconds": elapsed,
            "value": observed,
            "sha256": digest(executable),
            "usage": usage(target),
        }, result

    report["isolated_a"], _ = build(a, targets / "private-a")
    report["isolated_b"], _ = build(b, targets / "private-b")
    assert report["isolated_a"]["value"] == report["isolated_b"]["value"] == "one"
    one = targets / "private-a/debug" / binary_name
    two = targets / "private-b/debug" / binary_name
    assert one.stat().st_ino != two.stat().st_ino
    report["private_outputs_distinct_inodes"] = True
    protected = root / "controller"
    protected.mkdir(mode=0o700)
    protected_driver = protected / "driver"
    shutil.copyfile(one, protected_driver)
    protected_driver.chmod(0o555)
    protected_sha = digest(protected_driver)
    assert stat.S_IMODE(protected.stat().st_mode) == 0o700
    assert stat.S_IMODE(protected_driver.stat().st_mode) == 0o555
    if os.geteuid() == 0 and shutil.which("setpriv"):
        run(
            [
                "setpriv",
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "python3",
                "-c",
                "import sys\ntry: open(sys.argv[1],'wb').write(b'overwrite')\nexcept PermissionError: sys.exit(77)",
                str(protected_driver),
            ],
            root,
            env,
            expected=77,
        )
        report["separate_uid_cannot_write_private_controller"] = 65534
    else:
        report["separate_uid_control"] = (
            "unavailable in this process; explicit mode assertions remain, no cross-UID result inferred"
        )
    contexts = {}
    for domain, features in [("trusted", []), ("worker", []), ("worker", ["variant"])]:
        payload = {
            "compiler": report["rustc"],
            "flags": env["RUSTFLAGS"],
            "lock": digest(a / "Cargo.lock"),
            "profile": "dev",
            "trust_domain": domain,
            "features": features,
        }
        contexts[f"{domain}:{features}"] = hashlib.sha256(
            json.dumps(payload, sort_keys=True).encode()
        ).hexdigest()
    assert len(set(contexts.values())) == 3
    report["example_namespace_keys_not_admission_proof"] = contexts
    (b / "src/lib.rs").write_text(
        '#[cfg(feature="variant")] pub fn value() -> &\'static str { "variant" }\n#[cfg(not(feature="variant"))] '
        + library
    )
    report["namespaced_variant"], _ = build(
        b, targets / contexts["worker:['variant']"], ["--features", "variant"]
    )
    assert report["namespaced_variant"]["value"] == "variant"
    assert run([str(one)], a, env)[0].stdout.strip() == "one"
    (b / "src/lib.rs").write_text(library)
    report["incremental"], _ = build(
        a, targets / "incremental", override={"CARGO_INCREMENTAL": "1"}
    )
    report["incremental_bytes"] = usage(targets / "incremental/debug/incremental")
    shared = targets / "shared"
    report["shared_trusted_before"], _ = build(a, shared)
    (b / "src/lib.rs").write_text(library.replace('"one"', '"two"'))
    report["shared_worker_after"], _ = build(b, shared)
    assert report["shared_worker_after"]["value"] == "two"
    assert report["shared_trusted_before"]["sha256"] != digest(
        shared / "debug" / binary_name
    )
    report["shared_target_overwrites_trusted_executable"] = True
    assert run([str(one)], a, env)[0].stdout.strip() == "one"
    concurrent = targets / "concurrent"
    concurrent.mkdir()
    jobs = [
        subprocess.Popen(
            ["cargo", "build", "--offline"],
            cwd=project,
            env=env | {"CARGO_TARGET_DIR": str(concurrent)},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for project in [a, b]
    ]
    try:
        outcomes = [job.communicate(timeout=120) for job in jobs]
        assert all(job.returncode == 0 for job in jobs), outcomes
    finally:
        for job in jobs:
            if job.poll() is None:
                job.kill()
                job.communicate(timeout=10)
    assert run([str(concurrent / "debug" / binary_name)], a, env)[0].stdout.strip() in [
        "one",
        "two",
    ]
    report["concurrent_shared_builds_complete"] = True
    report["concurrent_lock_diagnostics"] = [
        text for _, text in outcomes if "file lock" in text
    ]
    cache_config = root / "private-sccache-config.toml"
    cache_config.write_text("")
    cache_env = env | {
        "SCCACHE_CONF": str(cache_config),
        "SCCACHE_DIR": str(root / "sccache"),
        "SCCACHE_CACHE_SIZE": "8K",
        "SCCACHE_IDLE_TIMEOUT": "60",
        "RUSTC_WRAPPER": shutil.which("sccache") or "sccache",
    }
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        cache_env["SCCACHE_SERVER_PORT"] = str(reserve.getsockname()[1])
    run(["sccache", "--start-server"], root, cache_env)
    initial = json.loads(
        run(["sccache", "--show-stats", "--stats-format=json"], root, cache_env)[
            0
        ].stdout
    )
    assert initial["cache_location"] == f'Local disk: "{root / "sccache"}"'
    assert initial["max_cache_size"] == 8192
    try:
        (b / "src/lib.rs").write_text(library)
        report["cached_a"], _ = build(a, targets / "cached-a", override=cache_env)
        shutil.rmtree(targets / "cached-a")
        report["cached_a_rematerialized"], _ = build(
            a, targets / "cached-a", override=cache_env
        )
        report["cached_a_fresh_target"], _ = build(
            a, targets / "cached-a-again", override=cache_env
        )
        report["cached_b"], _ = build(b, targets / "cached-b", override=cache_env)
        report["cache_growth_epochs"] = []
        for epoch in range(6):
            (a / "src/lib.rs").write_text(library.replace('"one"', f'"epoch-{epoch}"'))
            changed, _ = build(a, targets / "cached-a", override=cache_env)
            assert changed["value"] == f"epoch-{epoch}"
            stats = json.loads(
                run(
                    ["sccache", "--show-stats", "--stats-format=json"], root, cache_env
                )[0].stdout
            )
            assert stats["cache_size"] <= stats["max_cache_size"]
            report["cache_growth_epochs"].append(
                {
                    "epoch": epoch,
                    "cache_size": stats["cache_size"],
                    "max_cache_size": stats["max_cache_size"],
                }
            )
        entries = [
            path
            for path in (root / "sccache").rglob("*")
            if stat.S_ISREG(path.lstat().st_mode)
        ]
        assert entries
        for entry in entries:
            entry.write_bytes(b"deliberately corrupt private fixture cache")
        shutil.rmtree(targets / "cached-a")
        report["corrupt_cache_recomputed"], _ = build(
            a, targets / "cached-a", override=cache_env
        )
        assert report["corrupt_cache_recomputed"]["value"] == "epoch-5"
        stats = json.loads(
            run(["sccache", "--show-stats", "--stats-format=json"], root, cache_env)[
                0
            ].stdout
        )
        counters = stats["stats"]
        assert (
            counters["cache_read_errors"]
            + sum(counters["cache_errors"]["counts"].values())
            > 0
        )
        report["sccache"] = stats
        report["private_cache_usage"] = usage(root / "sccache")
        (a / "src/lib.rs").write_text(library)
    finally:
        run(["sccache", "--stop-server"], root, cache_env)
    (a / "notes.txt").write_text("preserved stash\n")
    run(["git", "stash", "push", "-qm", "Preserved synthetic recovery"], a, env)
    (a / "notes.txt").write_text("preserved dirty source\n")
    before = source_state(a, env)
    lease = root / "active-lease"
    with lease.open("w") as active:
        fcntl.flock(active, fcntl.LOCK_EX)
        probe = subprocess.run(
            [
                "python3",
                "-c",
                "import fcntl,sys\nf=open(sys.argv[1])\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(75)",
                str(lease),
            ],
            capture_output=True,
            text=True,
            timeout=5,
        )
        assert probe.returncode == 75, probe.stderr
        report["live_kernel_lease_rejects_collection"] = True
        assert one.exists()
    quarantine = targets / "quarantine"
    interrupted = subprocess.Popen(
        [
            "python3",
            "-c",
            "import fcntl,os,sys; lease=open(sys.argv[3],'w'); fcntl.flock(lease,fcntl.LOCK_EX|fcntl.LOCK_NB); os.rename(sys.argv[1],sys.argv[2]); print('quarantined',flush=True); sys.stdin.read()",
            str(targets / "private-b"),
            str(quarantine),
            str(root / "garbage-lease"),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert select.select([interrupted.stdout], [], [], 5)[0]
        assert interrupted.stdout.readline().strip() == "quarantined"
        interrupted.kill()
        interrupted.communicate(timeout=5)
        assert interrupted.returncode == -9
    finally:
        if interrupted.poll() is None:
            interrupted.kill()
            interrupted.communicate(timeout=5)
    report["restart_quarantine_present"] = quarantine.is_dir()
    assert quarantine.is_dir() and not (targets / "private-b").exists()
    assert shutil.rmtree.avoids_symlink_attacks
    run(
        [
            "python3",
            "-c",
            "import fcntl,shutil,sys; lease=open(sys.argv[2],'w'); fcntl.flock(lease,fcntl.LOCK_EX|fcntl.LOCK_NB); shutil.rmtree(sys.argv[1])",
            str(quarantine),
            str(root / "garbage-lease"),
        ],
        root,
        env,
    )
    assert not quarantine.exists()
    report["restart_completes_owned_quarantine_cleanup"] = True
    link = targets / "foreign-link"
    link.symlink_to(a, target_is_directory=True)
    assert stat.S_ISLNK(link.lstat().st_mode)
    link.unlink()
    assert source_state(a, env) == before
    assert digest(protected_driver) == protected_sha
    assert run([str(protected_driver)], a, env)[0].stdout.strip() == "one"
    assert run([str(one)], a, env)[0].stdout.strip() == "one"
    report["source_head_stash_dirty_notes_preserved"] = True
    report["independent_continuation"], _ = build(b, targets / "continuation")
    assert report["independent_continuation"]["value"] == "one"
    report["retained_targets_usage"] = usage(targets)
    report["limitations"] = [
        "Prototype only: no production collection policy is installed.",
        "Private fixture leases are not checkout-disposal or evidence authority.",
        "Killed collector and restart operate only on this experiment's generated private targets.",
        "No full host filesystem, container-engine or kernel-namespace acceptance is inferred.",
        "The synthetic 8KiB cache cap and example namespace keys are not production sizing or complete provenance.",
        "SIGKILL recovery is not power-loss durability; no durable production collector is implemented.",
    ]
    return report


def main():
    root = Path(tempfile.mkdtemp(prefix="loom-artifact-storage-"))
    root.chmod(0o700)
    try:
        report = experiment(root)
    except BaseException:
        print(f"failed private fixture retained at {root}", file=sys.stderr)
        raise
    Path("artifact-storage-report.json").write_text(json.dumps(report, indent=2) + "\n")
    shutil.rmtree(root)
    print(
        json.dumps(
            {
                "pass": True,
                "evidence": "bounded private artifact measurements in artifact-storage-report.json; not production cleanup authority",
            }
        )
    )


if __name__ == "__main__":
    main()
