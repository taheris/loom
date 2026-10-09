"""Execute the pinned Linux entrypoint with relocated files and external tools."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(sys.argv[1])
BASH = shutil.which("bash")
EXPECTED_ARGS = {
    "pi": ["--mode", "rpc"],
    "claude": [
        "--dangerously-skip-permissions",
        "--permission-prompt-tool",
        "stdio",
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ],
    "direct": [],
}
BINARIES = {"pi": "pi", "claude": "claude", "direct": "loom-direct-runner"}


class Fixture:
    def __init__(self, root, agent, mutation=None):
        self.root = root
        self.workspace = root / "workspace"
        self.home = root / "home"
        self.etc = root / "etc/wrix"
        self.tools = root / "tools"
        for directory in (self.workspace / "bin", self.home, self.etc, self.tools):
            directory.mkdir(parents=True)
        self.relocations = {
            "/workspace": str(self.workspace),
            "/home/wrix": str(self.home),
            "/etc/": str(root / "etc") + "/",
            "/tmp/wrix-": str(root / "wrix-"),
        }
        helpers = {
            "/git-ssh-setup.sh": "lib/util/git-ssh-setup.sh",
            "/beads-sandbox.sh": "lib/beads/sandbox.sh",
            "/network-ready.sh": "lib/sandbox/network-ready.sh",
            "/mcp-manifest.sh": "lib/sandbox/mcp-manifest.sh",
        }
        for target, source in helpers.items():
            self.relocations[target] = str(root / target.lstrip("/"))
        self.relocations["/proc/self/status"] = str(root / "capabilities")
        self.relocations["/run/wrix-network-ready"] = str(root / "network-ready")
        (root / "network-ready").touch()
        (root / "capabilities").write_text(
            "".join(f"{field}:\t0000000000000000\n" for field in
                    ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"))
        )
        for target, source in helpers.items():
            (root / target.lstrip("/")).write_text(self.relocate((SOURCE / source).read_text()))
        entrypoint = (SOURCE / "lib/sandbox/linux/entrypoint.sh").read_text()
        if mutation is not None:
            old, new = mutation
            if entrypoint.count(old) != 1:
                raise AssertionError(f"Mutation must match once: {old!r}")
            entrypoint = entrypoint.replace(old, new)
        self.entrypoint = root / "entrypoint.sh"
        self.entrypoint.write_text(self.relocate(entrypoint))
        (self.etc / "image-agent").write_text(agent)
        for filename in ("claude-config.json", "claude-settings.json"):
            (self.etc / filename).write_text('{"env":{}}\n')
        (self.workspace / ".beads").mkdir()
        (self.workspace / ".beads/metadata.json").write_text('{"backend":"dolt"}\n')
        (self.workspace / ".beads/config.yaml").write_text("sync.mode: dolt-native\n")
        (root / "deploy-key").touch()
        self.write_tool(self.tools / "git", """
import json, os, sys
with open(os.environ['FIXTURE_GIT_LOG'], 'a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
sys.exit(1 if '--get' in sys.argv else 0)
""")
        self.write_tool(self.tools / "getent", """
import os
print('wrix:x:1000:1000:Wrix:' + os.environ['HOME'] + ':/bin/bash')
""")
        self.write_tool(self.tools / "bd", """
import json, os, sys
assert sys.argv[1:] == ['--readonly', 'sql', 'SELECT 1'], sys.argv
assert os.environ['BEADS_DOLT_AUTO_START'] == '0'
assert os.environ['BD_IMPORT_AUTO'] == 'false'
with open(os.environ['FIXTURE_BD_LOG'], 'a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
""")
        for runtime, binary in BINARIES.items():
            self.write_tool(self.workspace / "bin" / binary, f"""
import json, os, sys
print(json.dumps({{
    'runtime': {runtime!r}, 'argv': sys.argv[1:],
    'ssh': os.environ.get('GIT_SSH_COMMAND'),
    'auto_start': os.environ.get('BEADS_DOLT_AUTO_START'),
    'import_auto': os.environ.get('BD_IMPORT_AUTO'),
    'endpoint': [os.environ.get('BEADS_DOLT_SERVER_HOST'), os.environ.get('BEADS_DOLT_SERVER_PORT')],
}}))
sys.exit(23)
""")
        self.env = {
            "PATH": str(self.tools) + os.pathsep + os.environ["PATH"],
            "HOME": str(self.home),
            "WRIX_AGENT": agent,
            "WRIX_STDIO": "1",
            "WRIX_DEPLOY_KEY": str(root / "deploy-key"),
            "BEADS_DOLT_SERVER_HOST": "192.0.2.1",
            "BEADS_DOLT_SERVER_PORT": "24470",
            "FIXTURE_GIT_LOG": str(root / "git.jsonl"),
            "FIXTURE_BD_LOG": str(root / "bd.jsonl"),
        }

    def relocate(self, body):
        for old, new in self.relocations.items():
            body = body.replace(old, new)
        return body

    @staticmethod
    def write_tool(path, body):
        path.write_text(f"#!{sys.executable}\n{body}")
        path.chmod(0o755)

    def run(self):
        return subprocess.run([BASH, str(self.entrypoint)], env=self.env,
                              capture_output=True, text=True, timeout=20, check=False)


class Entrypoint(unittest.TestCase):
    def check_runtime(self, agent, mutation=None):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory), agent, mutation)
            result = fixture.run()
            self.assertEqual(result.returncode, 23, result.stderr)
            observed = json.loads(result.stdout)
            self.assertEqual(observed["runtime"], agent)
            self.assertEqual(observed["argv"], EXPECTED_ARGS[agent])
            self.assertEqual((fixture.home / ".claude/settings.json").exists(), agent == "claude")
            self.assertEqual((fixture.home / ".pi/agent").exists(), agent == "pi")

    def check_shared_setup(self, mutation=None):
        for agent in BINARIES:
            with tempfile.TemporaryDirectory() as directory:
                fixture = Fixture(Path(directory), agent, mutation)
                result = fixture.run()
                self.assertEqual(result.returncode, 23, result.stderr)
                observed = json.loads(result.stdout)
                self.assertEqual(observed["runtime"], agent)
                self.assertIn(str(fixture.root / "deploy-key"), observed["ssh"] or "")
                self.assertIn("StrictHostKeyChecking=yes", observed["ssh"] or "")
                self.assertTrue((fixture.home / ".ssh/config").is_file())
                git_calls = [json.loads(line) for line in (fixture.root / "git.jsonl").read_text().splitlines()]
                self.assertIn(["config", "--global", "--replace-all", "core.sshCommand", observed["ssh"]], git_calls)
                self.assertEqual(observed["auto_start"], "0")
                self.assertEqual(observed["import_auto"], "false")
                self.assertEqual(observed["endpoint"], ["192.0.2.1", "24470"])
                self.assertTrue((fixture.root / "bd.jsonl").is_file())
                self.assertEqual((fixture.root / "bd.jsonl").read_text(), '["--readonly", "sql", "SELECT 1"]\n')
                logs = list((fixture.workspace / ".wrix/log").glob("*.json"))
                self.assertEqual(len(logs), 1)
                self.assertEqual(json.loads(logs[0].read_text())["exit_code"], 23)
                (fixture.root / "network-ready").unlink()
                rejected = fixture.run()
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("network bootstrap did not complete", rejected.stderr)
                self.assertEqual(rejected.stdout, "")
                (fixture.root / "network-ready").touch()
                (fixture.root / "capabilities").write_text("CapEff:\t0000000000001000\n")
                rejected = fixture.run()
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("NET_ADMIN survived", rejected.stderr)
                self.assertEqual(rejected.stdout, "")

    def test_pi_rpc(self):
        self.check_runtime("pi")

    def test_claude_stdio(self):
        self.check_runtime("claude")

    def test_direct_stdio(self):
        self.check_runtime("direct")

    def test_shared_setup(self):
        self.check_shared_setup()

    def test_pi_verifier_rejects_wrong_mode(self):
        with self.assertRaises(AssertionError):
            self.check_runtime("pi", ("pi --mode rpc", "pi --mode text"))

    def test_claude_verifier_rejects_missing_permission_tool(self):
        with self.assertRaises(AssertionError):
            self.check_runtime("claude", ("--permission-prompt-tool stdio", "--permission-prompt-tool wrong"))

    def test_claude_verifier_rejects_missing_bypass(self):
        with self.assertRaises(AssertionError):
            self.check_runtime("claude", ("    --dangerously-skip-permissions \\\n", ""))

    def test_direct_verifier_rejects_wrong_runtime(self):
        with self.assertRaises(AssertionError):
            self.check_runtime("direct", ("  loom-direct-runner || MAIN_EXIT=$?", "  pi || MAIN_EXIT=$?"))

    def test_shared_verifier_rejects_missing_ssh_setup(self):
        with self.assertRaises(AssertionError):
            self.check_shared_setup((". /git-ssh-setup.sh", ":"))

    def test_shared_verifier_rejects_missing_beads_readiness(self):
        with self.assertRaises(AssertionError):
            self.check_shared_setup(("wrix_wait_for_beads_endpoint\n", ":\n"))

    def test_shared_verifier_rejects_missing_network_guard(self):
        with self.assertRaises(AssertionError):
            self.check_shared_setup((". /network-ready.sh", ":"))


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *sys.argv[2:]])
