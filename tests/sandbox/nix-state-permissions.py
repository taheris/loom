"""Execute the worker image's permission initialization on a materialized tree."""

from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(sys.argv.pop(1))


class StatePermissions(unittest.TestCase):
    def test_initialization_secures_ancestors_without_disabling_local_store(self):
        image = (SOURCE / "lib/sandbox/image.nix").read_text()
        start = image.index("      chmod -R a+rwX nix/store nix/var/nix\n")
        end = image.index("      rm -f nix/var/nix/db/reserved\n", start)
        commands = image[start:end]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in ("nix/store", "nix/var/nix/db", "nix/var/nix/builds"):
                (root / relative).mkdir(parents=True, exist_ok=True)
            for relative in ("nix", "nix/var", "nix/var/nix"):
                (root / relative).chmod(0o777)
            database = root / "nix/var/nix/db/db.sqlite"
            database.touch(mode=0o444)
            for _ in range(2):
                subprocess.run(["bash", "-euo", "pipefail", "-c", commands],
                               cwd=root, check=True)
                for relative in ("nix", "nix/var", "nix/var/nix"):
                    self.assertEqual(stat.S_IMODE((root / relative).stat().st_mode),
                                     0o755, relative)
                for relative in ("nix/store", "nix/var/nix/db", "nix/var/nix/builds"):
                    self.assertEqual((root / relative).stat().st_mode & 0o777,
                                     0o777, relative)
                self.assertEqual(stat.S_IMODE(database.stat().st_mode), 0o666)
                self.assertEqual(stat.S_IMODE((root / "nix/var/nix/gcroots/auto").stat().st_mode),
                                 0o1777)


if __name__ == "__main__":
    unittest.main()
