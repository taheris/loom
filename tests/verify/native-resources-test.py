"""Independent controls for native execution-closure source partitioning."""

import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("native_resources", Path(__file__).with_name("native-resources.py"))
resources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resources)


def store(name):
    return f"/nix/store/{'a' * 32}-{name}"


def node(dependencies=(), sources=()):
    return {"version": 4, "inputs": {"drvs": {name: {} for name in dependencies}, "srcs": list(sources)}}


class NativeSourceClosure(unittest.TestCase):
    def setUp(self):
        self.source = {"origin": "assets/input.txt", "path": store("asset")}
        self.graph = {
            store("alpha.drv"): node([store("child.drv")]),
            store("child.drv"): node(sources=[store("asset")]),
            store("beta.drv"): node(),
        }

    def definition(self, name, sources=()):
        return {"root": store(name), "sources": list(sources), "evaluation": []}

    def test_actual_transitive_child_retains_its_constructor_provenance(self):
        self.assertEqual(resources.execution_sources(self.definition("alpha.drv"), self.graph, [self.source]), [self.source])

    def test_missing_flake_member_cannot_be_repaired_by_partition_fallback(self):
        with self.assertRaisesRegex(ValueError, "test-module.nix"):
            resources.require_control_plane({"flake.nix": "observed"}, {"flake.nix", "tests/test-module.nix"})

    def test_complete_actual_control_plane_is_retained(self):
        resources.require_control_plane({"flake.nix": "observed", "tests/test-module.nix": "observed"}, {"flake.nix", "tests/test-module.nix"})

    def test_readonly_recipe_source_retains_its_referenced_source_closure(self):
        self.graph[store("alpha.drv")] = node(sources=[store("child.drv")])
        self.assertEqual(resources.execution_sources(self.definition("alpha.drv"), self.graph, [self.source]), [self.source])

    def test_unrelated_root_cannot_borrow_a_siblings_source(self):
        self.assertEqual(resources.execution_sources(self.definition("beta.drv"), self.graph, [self.source]), [])

    def test_unused_declared_resource_is_rejected_not_counted_as_coverage(self):
        with self.assertRaisesRegex(ValueError, "not in the actual execution closure"):
            resources.execution_sources(self.definition("beta.drv", [self.source]), self.graph, [self.source])

    def test_missing_dependency_cannot_truncate_the_source_closure(self):
        del self.graph[store("child.drv")]
        with self.assertRaises(KeyError):
            resources.execution_sources(self.definition("alpha.drv"), self.graph, [self.source])

    def test_unknown_derivation_version_is_not_an_empty_success(self):
        self.graph[store("alpha.drv")]["version"] = 99
        with self.assertRaisesRegex(ValueError, "unsupported native derivation version"):
            resources.execution_sources(self.definition("alpha.drv"), self.graph, [self.source])

    def test_only_canonical_native_store_identities_are_accepted(self):
        for value in ["..", "../asset", "/tmp/asset", store("asset") + "/child", "e" * 32 + "-asset"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                resources.store_path(value)
        self.assertEqual(resources.store_path(Path(store("asset")).name), store("asset"))


class CargoStorage(unittest.TestCase):
    def setUp(self):
        self.root = store("terminal.drv")
        self.reporter = store("reporter.drv")
        self.catalogue = store("catalogue.drv")
        self.graph = {
            self.root: {"env": {"doInstallCargoArtifacts": ""}},
            self.reporter: {"env": {"doInstallCargoArtifacts": ""}},
            self.catalogue: {"env": {"doInstallCargoArtifacts": "1"}},
        }

    def test_terminal_cargo_operations_do_not_export_whole_targets(self):
        for operation in ["build", "lint", "lint-all-targets"]:
            definition = {"root": self.root, "cargo": operation}
            resources.require_cargo_storage(definition, self.graph)
            self.graph[self.root]["env"]["doInstallCargoArtifacts"] = "1"
            with self.assertRaisesRegex(ValueError, "whole-target export"):
                resources.require_cargo_storage(definition, self.graph)
            self.graph[self.root]["env"]["doInstallCargoArtifacts"] = ""

    def test_actual_report_producer_not_assertion_wrapper_controls_retention(self):
        definition = {"root": self.root, "reporter": self.reporter, "catalogue": self.catalogue, "cargo": "test"}
        resources.require_cargo_storage(definition, self.graph)
        self.graph[self.reporter]["env"]["doInstallCargoArtifacts"] = "1"
        with self.assertRaisesRegex(ValueError, "whole-target export"):
            resources.require_cargo_storage(definition, self.graph)

    def test_test_catalogue_retains_the_reusable_compiler_seed(self):
        definition = {"root": self.root, "reporter": self.reporter, "catalogue": self.catalogue, "cargo": "test"}
        self.graph[self.catalogue]["env"]["doInstallCargoArtifacts"] = ""
        with self.assertRaisesRegex(ValueError, "reusable compiler seed"):
            resources.require_cargo_storage(definition, self.graph)

    def test_missing_retention_setting_is_not_assumed_false(self):
        self.graph[self.root]["env"] = {}
        with self.assertRaisesRegex(ValueError, "whole-target export"):
            resources.require_cargo_storage({"root": self.root, "cargo": "build"}, self.graph)


if __name__ == "__main__":
    unittest.main()
