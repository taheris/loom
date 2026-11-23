"""Project the checked LLM workspace without changing any resolved package pins."""

import re
import sys
import tomllib
from pathlib import Path


def project(manifest):
    text = manifest.read_text()
    old = tomllib.loads(text)
    members = ["crates/loom-events", "crates/loom-llm"]
    if not set(members) <= set(old["workspace"]["members"]):
        raise ValueError("LLM workspace members missing from canonical manifest")
    projected, count = re.subn(
        r"(?ms)^members = \[.*?^\]", 'members = ["crates/loom-events", "crates/loom-llm"]', text
    )
    old["workspace"]["members"] = members
    if count != 1 or tomllib.loads(projected) != old:
        raise ValueError("expected only Cargo workspace membership to change")
    manifest.write_text(projected)


def pins(lock):
    return {
        (p["name"], p["version"], p.get("source"), p.get("checksum"))
        for p in tomllib.loads(lock.read_text())["package"]
    }


match sys.argv[1:]:
    case ["project", manifest]:
        project(Path(manifest))
    case ["check-lock", canonical, projected]:
        unexpected = pins(Path(projected)) - pins(Path(canonical))
        if unexpected:
            raise ValueError(f"projected Cargo lock changed pins: {unexpected!r}")
    case _:
        raise SystemExit("usage: project-llm-workspace.py project MANIFEST | check-lock CANONICAL PROJECTED")
