"""Phase 31.4 — end-to-end test of `scripts/pack-tarball-python.sh`.

Asserts the bash pipeline produces a tarball whose name + layout
+ sha256 sidecar exactly match the convention 31.1 consumes.

Synthetic SDK: a tempdir with a single `__init__.py` (one line)
substitutes for the real SDK so the test does not depend on the
in-tree SDK source layout. `requirements.txt` is empty, so no
real `pip install` runs (`SKIP_PIP=1` env override).
"""

import hashlib
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

TEMPLATE_ROOT = Path(__file__).resolve().parent.parent
PLUGIN_ID = "template_plugin_python"
PLUGIN_VERSION = "0.1.0"


class PackTarballTests(unittest.TestCase):
    def test_pack_tarball_produces_canonical_layout(self):
        with tempfile.TemporaryDirectory() as work_str, tempfile.TemporaryDirectory() as sdk_str, tempfile.TemporaryDirectory() as extract_str:
            work = Path(work_str)
            sdk = Path(sdk_str)
            extract_dir = Path(extract_str)

            # 1. Synthetic SDK with a single import-able file.
            sdk_pkg = sdk / "nexo_plugin_sdk"
            sdk_pkg.mkdir()
            (sdk_pkg / "__init__.py").write_text("# stub SDK\n")

            # 2. Copy template fixture (manifest + scripts + src)
            #    into a fresh work dir so the test does not write
            #    into the workspace.
            for name in ("nexo-plugin.toml", "requirements.txt"):
                shutil.copy(TEMPLATE_ROOT / name, work / name)
            shutil.copytree(TEMPLATE_ROOT / "src", work / "src")
            shutil.copytree(TEMPLATE_ROOT / "scripts", work / "scripts")

            # 3. Run the pack script with SDK_SRC + SKIP_PIP overrides.
            env = dict(os.environ)
            env["SDK_SRC"] = str(sdk_pkg)
            env["SKIP_PIP"] = "1"
            result = subprocess.run(
                ["bash", "scripts/pack-tarball-python.sh"],
                cwd=work,
                env=env,
                capture_output=True,
                check=False,
            )
            self.assertEqual(
                result.returncode,
                0,
                msg=f"pack failed:\nstdout={result.stdout!r}\nstderr={result.stderr!r}",
            )

            # 4. Asset present.
            asset = work / "dist" / f"{PLUGIN_ID}-{PLUGIN_VERSION}-noarch.tar.gz"
            sidecar = work / "dist" / f"{PLUGIN_ID}-{PLUGIN_VERSION}-noarch.tar.gz.sha256"
            self.assertTrue(asset.is_file(), f"asset missing: {asset}")
            self.assertTrue(sidecar.is_file(), f"sha sidecar missing: {sidecar}")

            sidecar_hex = sidecar.read_text().strip()
            self.assertEqual(len(sidecar_hex), 64, f"sha sidecar must be 64 hex chars, got {sidecar_hex!r}")
            self.assertTrue(
                all(c.isdigit() or c in "abcdef" for c in sidecar_hex),
                "sidecar must be lowercase hex",
            )

            # 5. Recompute sha256.
            sha = hashlib.sha256(asset.read_bytes()).hexdigest()
            self.assertEqual(sha, sidecar_hex, "sha256 mismatch vs sidecar")

            # 6. Re-extract and verify layout.
            with tarfile.open(asset, "r:gz") as tf:
                tf.extractall(extract_dir)

            expected_root = {"bin", "lib", "nexo-plugin.toml"}
            actual_root = {p.name for p in extract_dir.iterdir()}
            self.assertEqual(
                actual_root,
                expected_root,
                f"unexpected top-level entries: {actual_root - expected_root}",
            )
            self.assertTrue((extract_dir / "bin" / PLUGIN_ID).is_file())
            self.assertTrue((extract_dir / "lib" / "plugin" / "main.py").is_file())
            self.assertTrue(
                (extract_dir / "lib" / "nexo_plugin_sdk" / "__init__.py").is_file()
            )

            # Launcher is executable on Unix.
            mode = (extract_dir / "bin" / PLUGIN_ID).stat().st_mode & 0o777
            self.assertEqual(mode, 0o755, f"launcher mode should be 0o755, got {mode:o}")


if __name__ == "__main__":
    unittest.main()
