#!/usr/bin/env python3
"""Measure compiler-cache context and trust boundaries in fresh private fixtures."""

import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time


LIMIT = 256 * 1024 * 1024
BASE_FLAGS = "-C debuginfo=0 --check-cfg=cfg(cache_variant)"
LIBRARY = """#[cfg(feature="variant")]
pub fn value() -> &'static str { "feature" }
#[cfg(all(not(feature="variant"),cache_variant))]
pub fn value() -> &'static str { "flag" }
#[cfg(all(not(feature="variant"),not(cache_variant)))]
pub fn value() -> &'static str { "one" }
"""


def command(argv, cwd, env):
    started = time.monotonic()
    result = subprocess.run(
        argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=120
    )
    assert result.returncode == 0, (argv, result.returncode, result.stderr)
    return result, round(time.monotonic() - started, 4)


def allocated(root):
    seen = set()
    size = 0
    for path in root.rglob("*"):
        info = path.lstat()
        key = (info.st_dev, info.st_ino)
        if stat.S_ISREG(info.st_mode) and key not in seen:
            size += info.st_blocks * 512
            seen.add(key)
    assert size <= LIMIT, (size, LIMIT)
    return size


def environment(root):
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("GIT_", "SCCACHE_"))
    }
    for key in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "SCCACHE_BASEDIRS",
    ]:
        env.pop(key, None)
    home = root / "home"
    home.mkdir(mode=0o700)
    env.update(
        HOME=str(home),
        CARGO_HOME=str(root / "cargo-home"),
        CARGO_INCREMENTAL="0",
        CARGO_BUILD_JOBS="1",
        RUSTFLAGS=BASE_FLAGS,
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_SYSTEM="/dev/null",
    )
    return env


def project(root, env):
    root.mkdir(mode=0o700)
    (root / "src").mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname="rust-cache-context-fixture"\nversion="0.0.0"\nedition="2024"\n'
        "[workspace]\n[features]\nvariant=[]\n"
    )
    (root / "src/lib.rs").write_text(LIBRARY)
    (root / "src/main.rs").write_text(
        'fn main() { println!("{}",rust_cache_context_fixture::value()); }\n'
    )
    (root / ".gitignore").write_text("/target/\n")
    command(["git", "init", "-q"], root, env)
    command(["git", "add", "."], root, env)
    command(
        [
            "git",
            "-c",
            "user.name=Cache fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Synthetic cache fixture",
        ],
        root,
        env,
    )


class CompilerCache:
    def __init__(self, root, env, normalized=()):
        self.root = root
        config = root / "private-sccache-config.toml"
        config.write_text("")
        self.env = env | {
            "SCCACHE_CONF": str(config),
            "SCCACHE_DIR": str(root / "cache"),
            "SCCACHE_CACHE_SIZE": "128K",
            "SCCACHE_IDLE_TIMEOUT": "60",
            "RUSTC_WRAPPER": shutil.which("sccache"),
        }
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            self.env["SCCACHE_SERVER_PORT"] = str(reserve.getsockname()[1])
        if normalized:
            self.env["SCCACHE_BASEDIRS"] = os.pathsep.join(map(str, normalized))

    def __enter__(self):
        command(["sccache", "--start-server"], self.root, self.env)
        observed = self.stats()
        assert observed["cache_location"] == f'Local disk: "{self.root / "cache"}"'
        assert observed["max_cache_size"] == 128 * 1024
        return self

    def __exit__(self, *_):
        command(["sccache", "--stop-server"], self.root, self.env)

    def stats(self):
        output, _ = command(
            ["sccache", "--show-stats", "--stats-format=json"], self.root, self.env
        )
        value = json.loads(output.stdout)
        if value["cache_size"] is None:
            assert not self.files(), value
        else:
            assert value["cache_size"] <= value["max_cache_size"]
        return value

    def files(self):
        return {
            path
            for path in (self.root / "cache").rglob("*")
            if stat.S_ISREG(path.lstat().st_mode)
        }

    def build(
        self, source, target, transport="argument", flags=BASE_FLAGS, features=()
    ):
        target.mkdir(mode=0o700, exist_ok=True)
        assert stat.S_IMODE(target.stat().st_mode) == 0o700
        env = self.env | {"RUSTFLAGS": flags}
        argv = ["cargo", "build", "--offline", "-vv"]
        if transport == "environment":
            env["CARGO_TARGET_DIR"] = str(target)
        else:
            assert transport == "argument"
            argv += ["--target-dir", str(target)]
        if features:
            argv += ["--features", ",".join(features)]
        before = self.stats()["stats"]["cache_hits"]["counts"].get("Rust", 0)
        output, seconds = command(argv, source, env)
        executable = target / "debug/rust-cache-context-fixture"
        value = command([str(executable)], source, env)[0].stdout.strip()
        after = self.stats()["stats"]["cache_hits"]["counts"].get("Rust", 0)
        allocated(self.root)
        # Cargo's explicit rustc assignments/arguments, not the inherited environment.
        compiler = [
            line.strip()
            for line in output.stderr.splitlines()
            if "Running `" in line
            and "--crate-type lib " in line
            and "--crate-name rust_cache_context_fixture " in line
        ]
        assert len(compiler) == 1, output.stderr
        return {
            "value": value,
            "seconds": seconds,
            "rust_cache_hits_added": after - before,
            "target_allocated_file_bytes": allocated(target),
            "rustc_invocation": compiler[0],
        }


def context_case(root, transport, normalize):
    root.mkdir(mode=0o700)
    env = environment(root)
    a, b = root / "a", root / "b"
    project(a, env)
    command(["git", "clone", "-q", "--no-hardlinks", str(a), str(b)], root, env)
    with CompilerCache(root, env, (a, b) if normalize else ()) as cache:
        target = root / "target-one"
        first = cache.build(a, target, transport)
        assert first["value"] == "one"
        shutil.rmtree(target)
        rematerialized = cache.build(a, target, transport)
        assert rematerialized["value"] == "one"
        assert rematerialized["rust_cache_hits_added"] == 1
        moved = cache.build(a, root / "target-two", transport)
        assert moved["value"] == "one"
        cloned = cache.build(b, root / "target-three", transport)
        assert cloned["value"] == "one"
        (a / "src/lib.rs").write_text(LIBRARY.replace('"one"', '"two"'))
        shutil.rmtree(target)
        changed = cache.build(a, target, transport)
        assert changed["value"] == "two"
        (a / "src/lib.rs").write_text(LIBRARY)
        shutil.rmtree(target)
        flagged = cache.build(
            a, target, transport, flags=BASE_FLAGS + " --cfg cache_variant"
        )
        assert flagged["value"] == "flag"
        shutil.rmtree(target)
        featured = cache.build(a, target, transport, features=("variant",))
        assert featured["value"] == "feature"
        stats = cache.stats()
        assert not normalize or stats["basedirs"]
        return {
            "transport": transport,
            "normalization_requested": normalize,
            "first": first,
            "same_target_rematerialized": rematerialized,
            "moved_target": moved,
            "cloned_source": cloned,
            "changed_source": changed,
            "changed_flags": flagged,
            "changed_features": featured,
            "sccache": stats,
        }


def macros(root):
    root.mkdir(mode=0o700)
    env = environment(root)
    a, b = root / "a", root / "b"
    project(a, env)
    command(["git", "clone", "-q", "--no-hardlinks", str(a), str(b)], root, env)
    for source in (a, b):
        (source / "src/lib.rs").write_text(
            'pub fn value() -> &\'static str { concat!(env!("CARGO_MANIFEST_DIR"),"|") }\n'
        )
        (source / "src/main.rs").write_text(
            'fn main() { println!("{}{}",rust_cache_context_fixture::value(),'
            'option_env!("CARGO_TARGET_DIR").unwrap_or("unset")); }\n'
        )
    results = []
    with CompilerCache(root, env, (a, b)) as cache:
        for source, name, transport in [
            (a, "one", "environment"),
            (a, "two", "environment"),
            (a, "three", "argument"),
            (b, "four", "argument"),
        ]:
            target = root / name
            result = cache.build(source, target, transport)
            expected = f"{source}|" + (
                str(target) if transport == "environment" else "unset"
            )
            assert result["value"] == expected
            results.append(result)
    return results


def dependency_control(root):
    root.mkdir(mode=0o700)
    env = environment(root)
    source = root / "a"
    project(source, env)
    manifest = source / "Cargo.toml"
    manifest.write_text(
        manifest.read_text()
        + '[dependencies]\ncache-context-dependency={path="dependency"}\n'
    )
    dependency = source / "dependency"
    (dependency / "src").mkdir(parents=True)
    dependency_manifest = dependency / "Cargo.toml"
    dependency_manifest.write_text(
        '[package]\nname="cache-context-dependency"\nversion="0.0.0"\nedition="2024"\n'
    )
    body = dependency / "src/lib.rs"
    body.write_text('pub fn value() -> &\'static str { "dependency-one" }\n')
    (source / "src/lib.rs").write_text(
        "pub fn value() -> &'static str { cache_context_dependency::value() }\n"
    )
    target = root / "target"
    with CompilerCache(root, env) as cache:
        first = cache.build(source, target)
        assert first["value"] == "dependency-one"
        lock_before = (source / "Cargo.lock").read_text()
        body.write_text('pub fn value() -> &\'static str { "dependency-two" }\n')
        shutil.rmtree(target)
        changed_body = cache.build(source, target)
        assert changed_body["value"] == "dependency-two"
        assert (source / "Cargo.lock").read_text() == lock_before
        dependency_manifest.write_text(
            dependency_manifest.read_text().replace(
                'version="0.0.0"', 'version="0.0.1"'
            )
        )
        shutil.rmtree(target)
        changed_version = cache.build(source, target)
        assert changed_version["value"] == "dependency-two"
        assert (source / "Cargo.lock").read_text() != lock_before
        return {
            "first": first,
            "changed_dependency_body": changed_body,
            "changed_dependency_version_and_lock": changed_version,
            "sccache": cache.stats(),
        }


def coherent_cache_control(root):
    root.mkdir(mode=0o700)
    env = environment(root)
    source = root / "a"
    project(source, env)
    target = root / "target"
    with CompilerCache(root, env) as cache:
        assert cache.build(source, target)["value"] == "one"
        first = cache.files()
        assert len(first) == 1
        original_path = next(iter(first))
        original_bytes = original_path.read_bytes()
        protected = root / "separately-retained-executable"
        shutil.copyfile(target / "debug/rust-cache-context-fixture", protected)
        protected.chmod(0o555)
        (source / "src/lib.rs").write_text(LIBRARY.replace('"one"', '"two"'))
        shutil.rmtree(target)
        assert cache.build(source, target)["value"] == "two"
        added = cache.files() - first
        assert len(added) == 1
        replacement = next(iter(added)).read_bytes()
        assert replacement != original_bytes
        original_path.write_bytes(replacement)
        (source / "src/lib.rs").write_text(LIBRARY)
        shutil.rmtree(target)
        observed = cache.build(source, target)
        assert observed["value"] in {"one", "two"}
        # A coherent but wrong compiled object must never count as correct source evidence.
        semantic_pass = observed["value"] == "one"
        assert command([str(protected)], root, env)[0].stdout.strip() == "one"
        original_path.write_bytes(original_bytes)
        shutil.rmtree(target)
        recovered = cache.build(source, target)
        assert recovered["value"] == "one"
        return {
            "scope": "only freshly generated private fixture cache entries were replaced",
            "source_expected": "one",
            "observed": observed,
            "fresh_semantic_comparison_passes": semantic_pass,
            "separately_retained_executable_unchanged": True,
            "original_private_cache_restored": recovered,
            "sccache": cache.stats(),
        }


def main():
    root = Path(tempfile.mkdtemp(prefix="loom-rust-cache-context-"))
    root.chmod(0o700)
    try:
        ignored_config = root / "ignored-parent-cache.toml"
        ignored_config.write_text("deliberately invalid parent configuration [")
        forbidden_cache = root / "must-not-use-parent-cache"
        os.environ.update(
            SCCACHE_CONF=str(ignored_config),
            SCCACHE_DIR=str(forbidden_cache),
            SCCACHE_SERVER_UDS=str(root / "must-not-connect-parent.sock"),
        )
        env = environment(root)
        report = {
            "rustc": command(["rustc", "--version", "--verbose"], root, env)[0].stdout,
            "cargo": command(["cargo", "--version", "--verbose"], root, env)[0].stdout,
            "cases": [
                context_case(root / f"{transport}-{normalize}", transport, normalize)
                for transport in ("environment", "argument")
                for normalize in (False, True)
            ],
            "observable_macros": macros(root / "macros"),
            "changed_dependency": dependency_control(root / "dependency"),
            "coherent_private_cache_control": coherent_cache_control(
                root / "coherent-control"
            ),
            "allocated_file_bytes": allocated(root),
            "limitations": [
                "Synthetic pinned fixtures; not a universal workload hit rate or production benchmark.",
                "No production cache policy, key hashing, mount or trust-domain behavior is changed.",
                "Normalization is measured, never assumed to preserve arbitrary macro/dependency semantics.",
                "Compiled cache results are not fresh verifier results, provenance or publication authority.",
            ],
        }
        assert not forbidden_cache.exists()
        assert (
            ignored_config.read_text() == "deliberately invalid parent configuration ["
        )
        report["inherited_cache_configuration_and_socket_not_selected"] = True
        for normalized in (False, True):
            by_transport = {
                case["transport"]: case
                for case in report["cases"]
                if case["normalization_requested"] == normalized
            }
            assert (
                by_transport["argument"]["moved_target"]["rust_cache_hits_added"]
                >= by_transport["environment"]["moved_target"]["rust_cache_hits_added"]
            )
    except BaseException:
        print(f"failed private cache fixture retained at {root}", file=sys.stderr)
        raise
    Path("rust-cache-context-report.json").write_text(
        json.dumps(report, indent=2) + "\n"
    )
    shutil.rmtree(root)
    print(
        json.dumps(
            {
                "pass": True,
                "evidence": "actual private compiler context and trust-boundary controls; see rust-cache-context-report.json",
            }
        )
    )


if __name__ == "__main__":
    main()
