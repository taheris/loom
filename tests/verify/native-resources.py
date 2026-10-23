"""Reevaluate the shipped workspace derivations in their exact native source partitions."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def nix(*args):
    environment = {key: value for key, value in os.environ.items() if key not in ("NIX_CONFIG", "NIX_PATH")}
    result = subprocess.run(
        ["nix", *args, "--extra-experimental-features", "nix-command flakes", "--option", "accept-flake-config", "false"],
        env=environment,
        check=True,
        capture_output=True,
        text=True,
        timeout=120,
    )
    return json.loads(result.stdout)


def metadata(root, tracked=False):
    reference = f"git+file://{root}" if tracked else f"path:{root}"
    return nix("flake", "metadata", reference, "--json", "--offline", "--no-write-lock-file")


def evaluate_many(project, system, targets):
    locked = json.dumps(project["url"]).replace("${", "\\${")
    requests = json.dumps(json.dumps(targets)).replace("${", "\\${")
    expression = (
        f"let flake = builtins.getFlake {locked}; system = {json.dumps(system)}; "
        "select = target: let original = flake.${target.namespace}.${system}.${target.attribute}; "
        'value = if target.namespace == "apps" then original.meta.loom.definition else original; in '
        '{ app = if target.namespace == "apps" then original.program else null; root = value.drvPath; '
        'sources = value.loom.sources; evaluation = value.loom.evaluation; version = value.loom.version; '
        'cargo = value.loom.cargo or null; reporter = value.loom.reporter.drvPath or null; catalogue = value.loom.catalogue or null; runtime = value.loom.runtime or "nix"; }; '
        f"in map select (builtins.fromJSON {requests})"
    )
    return nix("eval", "--json", "--offline", "--no-write-lock-file", "--expr", expression,
               "--option", "allow-import-from-derivation", "false")


def definitions(project, system):
    locked = json.dumps(project["url"]).replace("${", "\\${")
    expression = (
        f"let flake = builtins.getFlake {locked}; system = {json.dumps(system)}; "
        'collect = namespace: let values = flake.${namespace}.${system} or {}; '
        'registered = name: builtins.isAttrs values.${name} && '
        '(if namespace == "apps" then (values.${name}.meta or {}) ? loom else values.${name} ? loom); in '
        'assert builtins.all registered (builtins.attrNames values); '
        'map (attribute: { inherit namespace attribute; }) '
        '(builtins.filter registered (builtins.attrNames values)); '
        'in collect "packages" ++ collect "checks" ++ collect "apps"'
    )
    return nix("eval", "--json", "--offline", "--no-write-lock-file", "--expr", expression,
               "--option", "allow-import-from-derivation", "false")


def store_path(value):
    path = Path(value)
    if not re.fullmatch(r"[0-9abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+._?=-]+", path.name):
        raise ValueError(f"invalid native store identity: {value}")
    if path.is_absolute():
        if path.parent != Path("/nix/store"):
            raise ValueError(f"noncanonical native store path: {value}")
        return str(path)
    if path.name != value:
        raise ValueError(f"noncanonical native store name: {value}")
    return f"/nix/store/{value}"


def execution_sources(definition, graph, pool):
    pending = [definition["root"]]
    visited, referenced = set(), set()
    while pending:
        path = store_path(pending.pop())
        if path in visited:
            continue
        visited.add(path)
        node = graph[path]
        if node["version"] != 4:
            raise ValueError(f"unsupported native derivation version: {path}")
        source_paths = list(map(store_path, node["inputs"]["srcs"]))
        referenced.update(source_paths)
        pending.extend(node["inputs"]["drvs"])
        pending.extend(source for source in source_paths if source in graph)
    for source in definition["sources"]:
        if source["path"] not in referenced:
            raise ValueError(f"declared source is not in the actual execution closure: {source}")
    sources = {(source["origin"], source["path"]): source for source in definition["sources"]}
    for source in pool:
        if source["path"] in referenced:
            sources[(source["origin"], source["path"])] = source
    return list(sources.values())


def require_cargo_storage(definition, graph):
    operation = definition.get("cargo")
    if operation is None:
        return
    executable = store_path(definition.get("reporter") or definition["root"])
    expected = "1" if operation == "catalogue" else ""
    if graph[executable]["env"].get("doInstallCargoArtifacts") != expected:
        raise ValueError(f"Cargo {operation} has an unexpected whole-target export: {executable}")
    if operation == "test":
        catalogue = store_path(definition["catalogue"])
        if graph[catalogue]["env"].get("doInstallCargoArtifacts") != "1":
            raise ValueError(f"Cargo test catalogue lost its reusable compiler seed: {catalogue}")


def source_files(source):
    origin = Path(source["origin"])
    if origin.is_absolute() or ".." in origin.parts:
        raise ValueError(f"invalid native origin: {origin}")
    native = Path(store_path(source["path"]))
    for entry in sorted(native.rglob("*")) if native.is_dir() else [native]:
        if entry.is_symlink():
            raise ValueError(f"symlink in native source: {entry}")
        if entry.is_file():
            yield origin / entry.relative_to(native) if native.is_dir() else origin, entry


def require_control_plane(inputs, required):
    missing = set(required) - set(inputs)
    if missing:
        raise ValueError(f"native flake source duties lack declared evaluator/runtime members: {sorted(map(str, missing))}")


def main():
    root = Path(sys.argv[1] if len(sys.argv) == 2 else ".").resolve()
    project = metadata(root, tracked=True)
    captured = Path(project["path"])
    project = metadata(captured)
    system = nix("config", "show", "--json")["system"]["value"]
    targets = definitions(project, system)
    declared = evaluate_many(project, system, targets) if targets else []
    if len(declared) != len(targets):
        raise ValueError("native metadata omitted requested targets")
    pool = [source for definition in declared for source in definition["sources"] + definition["evaluation"]]
    graph = {}
    if declared:
        document = nix("derivation", "show", "--recursive", *sorted({definition["root"] for definition in declared}))
        if document["version"] != 4:
            raise ValueError("unsupported native derivation document")
        graph = {store_path(path): node for path, node in document["derivations"].items()}
    required_control = {path.relative_to(captured) for path in captured.rglob("*.nix")}
    reports, groups = [], {}
    for target, definition in zip(targets, declared):
        namespace, attribute = target["namespace"], target["attribute"]
        address = f"{namespace}.{system}.{attribute}"
        print(f"Checking native source partition: {address}", file=sys.stderr, flush=True)
        if definition["version"] != 1 or not (definition["sources"] or definition["evaluation"]):
            raise ValueError(f"missing checked sources: {address}")
        require_cargo_storage(definition, graph)
        inputs = {}
        for source in execution_sources(definition, graph, pool) + definition["evaluation"]:
            for origin, native in source_files(source):
                expected = captured / origin
                if expected.read_bytes() != native.read_bytes() or bool(expected.stat().st_mode & 0o111) != bool(native.stat().st_mode & 0o111):
                    raise ValueError(f"native source origin mismatch: {attribute}: {origin}")
                inputs[origin] = native
        runtime_files = len(inputs)
        if Path("rust-toolchain.toml") not in inputs:
            raise ValueError(f"missing actual flake toolchain evaluation input: {address}")
        if (
            attribute in ("verification-clippy-all-targets", "verification-nextest")
            and Path(".gitignore") not in inputs
        ):
            raise ValueError(f"staged Git discovery is missing its ignore input: {attribute}")
        require_control_plane(inputs, required_control)
        if Path("flake.lock") not in inputs:
            raise ValueError(f"missing declared flake lock evaluation input: {address}")
        scope = tuple(sorted((str(origin), str(native)) for origin, native in inputs.items()))
        group = groups.setdefault(scope, {"inputs": inputs, "targets": [], "definitions": []})
        group["targets"].append(target)
        group["definitions"].append(definition)
        reports.append({"namespace": namespace, "target": attribute, "system": system, "derivation": definition["root"], "runtime_files": runtime_files, "partition_files": len(inputs)})
    for group in groups.values():
        inputs = group["inputs"]
        print(f"Reevaluating independent native partition: {group['targets']}", file=sys.stderr, flush=True)
        staged = Path(tempfile.mkdtemp(prefix="loom-native-partition-"))
        try:
            for origin, native in inputs.items():
                destination = staged / origin
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(native, destination)
            isolated = evaluate_many(metadata(staged), system, group["targets"])
            if isolated != group["definitions"]:
                raise ValueError(f"native source partition changes derivation: {group['targets']}")
        except BaseException:
            print(f"failed native partition retained at {staged}", file=sys.stderr)
            raise
        shutil.rmtree(staged)
    print(json.dumps(reports, indent=2))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise
