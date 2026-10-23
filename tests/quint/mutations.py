"""Type-preserving mutations of real Quint IR, executed by its pinned Rust backend."""

import copy
import json
import pathlib
import subprocess
import sys
import unittest


def nodes(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from nodes(child)


def one(values):
    values = list(values)
    if len(values) != 1:
        raise ValueError(f"expected one mutation target, found {len(values)}")
    return values[0]


def definition(program, name):
    module = one(m for m in program["modules"] if m["name"] == "gate")
    return one(d for d in module["declarations"] if d.get("name") == name)


def app(node, opcode):
    return node.get("kind") == "app" and node.get("opcode") == opcode


def field_update(node, name):
    return app(node, "with") and node["args"][1].get("value") == name


def fresh_id(program):
    return 1 + max(n.get("id", 0) for n in nodes(program))


def replace(program, old, replacement):
    replacement = dict(replacement, id=old["id"])
    matches = [n for n in nodes(program["table"])
               if n.get("id") == old["id"] and n.get("kind") == old["kind"]]
    if not matches:
        raise ValueError("mutation target absent from resolved lookup table")
    for node in matches:
        if node != old:
            raise ValueError("inconsistent copies of mutation target")
        node.clear()
        node.update(copy.deepcopy(replacement))


class ModelMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = pathlib.Path(IR_PATH).read_text()
        cls.program = json.loads(cls.source)
        if cls.program["errors"]:
            raise ValueError("mutation input did not typecheck")

    def execute(self, program, name, expected):
        test = one(d for d in program["table"].values() if d.get("name") == name)
        request = json.dumps({"name": name, "test_def": test, "table": program["table"],
                              "seed": 42, "max_samples": 32, "verbosity": 0})
        try:
            result = subprocess.run([BACKEND, "test-from-stdin"], input=request,
                                    text=True, capture_output=True, timeout=15, check=True)
            output = json.loads(result.stdout)
            self.assertEqual(output["name"], name)
            self.assertEqual(output["status"], expected, output.get("errors"))
            self.assertGreater(output["nsamples"], 0)
            if expected == "passed":
                self.assertEqual(output["errors"], [])
            else:
                # A named expect assertion, not parser/runtime/provisioning failure.
                self.assertEqual([e["code"] for e in output["errors"]], ["QNT508"])
        except Exception:
            pathlib.Path(FAILURE_PATH).write_text(request)
            raise

    def rejects(self, name, mutate):
        self.execute(self.program, name, "passed")
        mutant = json.loads(self.source)
        mutate(mutant)
        self.execute(mutant, name, "failed")

    def test_witness_rejects_always_reject_model(self):
        def mutate(program):
            target = one(n for n in nodes(definition(program, "attemptPush"))
                         if field_update(n, "accepted"))["args"][2]
            self.assertEqual(target["name"], "authority")
            replace(program, target, {"kind": "bool", "value": False})
        self.rejects("witnessPublicationTest", mutate)

    def test_witness_rejects_stale_signer_trust(self):
        def mutate(program):
            target = one(n for n in nodes(definition(program, "admissible"))
                         if app(n, "or") and
                         {"imported", "signer"} <= {c.get("value") for c in nodes(n)
                                                   if c.get("kind") == "str"})
            replace(program, target, {"kind": "bool", "value": True})
        self.rejects("cacheTrustReadmissionTest", mutate)

    def test_witness_rejects_resource_construction_omission(self):
        def mutate(program):
            target = one(n for n in nodes(definition(program, "manifest")) if app(n, "union"))
            self.assertEqual(target["args"][1]["opcode"], "Set")
            replace(program, target, target["args"][0])
        self.rejects("referenceResourceOmissionDetectedTest", mutate)

    def test_witness_rejects_wrong_subject_responsibility(self):
        def mutate(program):
            comparison = one(n for n in nodes(definition(program, "unit")) if app(n, "ilt"))
            target = comparison["args"][1]
            self.assertEqual((target["kind"], target["value"]), ("int", 2))
            replace(program, target, {"kind": "int", "value": 1})
        # Only the positive independent construction comparison detects a bad unit.
        self.rejects("referenceResourceOmissionDetectedTest", mutate)

    def test_witness_rejects_passing_readiness_as_coverage(self):
        def mutate(program):
            target = one(n for n in nodes(definition(program, "complete")) if app(n, "Set"))
            self.assertEqual([n["value"] for n in target["args"]], ["pass", "pending"])
            modified = copy.deepcopy(target)
            modified["args"].append({"kind": "str", "id": fresh_id(program),
                                     "value": "UnneededPendingMarker"})
            replace(program, target, modified)
        self.rejects("referencePassingReadinessBlocksPublicationTest", mutate)

    def test_witness_rejects_worker_canonical_write_permission(self):
        def mutate(program):
            target = definition(program, "trustedWriter")["expr"]
            self.assertTrue(app(target, "contains"))
            replace(program, target, {"kind": "bool", "value": True})
        self.rejects("cacheWorkerCanonicalMutatorsRejectedTest", mutate)

    def test_witness_rejects_nondurable_checkpoint_deletion(self):
        def mutate(program):
            target = one(n for n in nodes(definition(program, "consolidate"))
                         if field_update(n, "stagedCheckpoint"))
            self.assertEqual(target["args"][2]["name"], "events")
            inner = copy.deepcopy(target)
            inner["id"] = fresh_id(program)
            inner["args"][1] = {"kind": "str", "id": inner["id"] + 1, "value": "checkpoint"}
            modified = copy.deepcopy(target)
            modified["args"][0] = inner
            replace(program, target, modified)
        self.rejects("cacheStagedCheckpointCannotPermitDeletionTest", mutate)


if __name__ == "__main__":
    IR_PATH, BACKEND, FAILURE_PATH = sys.argv[1:]
    unittest.main(argv=[sys.argv[0]], verbosity=2)
