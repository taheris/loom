"""External Wrix/Pi fixture: inspect the actual spawn contract, then mutate it."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

record, mode, *args = sys.argv[1:]
config = json.loads(Path(args[args.index("--spawn-config") + 1]).read_text())
workspace = Path(config["workspace"])
assert workspace.is_absolute()
assert Path(config["scratch_dir"]).is_relative_to(workspace)
assert not config.get("mounts", [])
assert (workspace / "src/lib.rs").read_text() == "original fixture\n"
assert (workspace / "binary").read_bytes() == bytes([0, 255, 0])
assert not (workspace / "other-side").exists()
assert not (workspace / ".beads").exists()
assert subprocess.check_output(["git", "remote"], cwd=workspace) == b""
head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=workspace).decode().strip()
print("[wrix] Starting container (mock)...", file=sys.stderr, flush=True)
probe = json.loads(input())
assert probe["type"] == "get_state"
print(json.dumps({"type": "response", "id": probe["id"], "command": "get_state", "success": True,
                  "data": {"isStreaming": False, "isCompacting": False, "messageCount": 0, "pendingMessageCount": 0}}), flush=True)
prompt = json.loads(input())
assert prompt["type"] == "prompt"
assert "fixture task context" in prompt["message"]
assert "original fixture" not in prompt["message"]
(workspace / "src/lib.rs").write_text("mutated\n")
(workspace / "other-side").write_text("private\n")
if mode == "regress" and "Tuned Guidance" in prompt["message"]:
    (workspace / "forbidden").write_text("unreported unrelated edit\n")
    subprocess.run(["git", "add", "src/lib.rs", "other-side", "forbidden"], cwd=workspace, check=True)
    subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                    "-c", "commit.gpgSign=false", "commit", "-qm", "hide changes from status"], cwd=workspace, check=True)
entry = {"workspace": str(workspace), "head": head, "pid": os.getpid()}
if mode == "hang":
    child = os.fork()
    if child == 0:
        time.sleep(3)
        Path(record + ".escaped").write_text("descendant survived cancellation")
        sys.exit(0)
    entry["child"] = child
with open(record, "a") as log:
    log.write(json.dumps(entry) + "\n")
if mode == "hang":
    time.sleep(60)
if mode == "fail":
    sys.exit(17)
print(json.dumps({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "text": "src/lib.rs\nLOOM_COMPLETE"}}), flush=True)
print(json.dumps({"type": "agent_end", "messages": []}), flush=True)
