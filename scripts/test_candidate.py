"""Verify candidate isolation without starting an application or using real data."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class CandidateLauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="candidate test ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(Path(__file__).with_name("run-candidate.sh"),
                        self.root / "scripts/run-candidate.sh")
        self.env = dict(os.environ, CHATCMD_BIND="0.0.0.0", CHATCMD_PORT="8080",
                        CHATCMD_DB_PATH="/must-not-use/stable.db",
                        CHATCMD_LOG_PATH="/must-not-use/stable.log")

    def fake_binary(self):
        target = self.root / "target/debug"
        target.mkdir(parents=True)
        binary = target / "chat-cmd-client"
        binary.write_text('#!/usr/bin/env python3\nimport json, os\n'
                          'print(json.dumps({k: v for k, v in os.environ.items() '
                          'if k.startswith("CHATCMD_")} | {"cwd": os.getcwd()}))\n')
        binary.chmod(0o755)

    def run_script(self, *args):
        return subprocess.run(["bash", str(self.root / "scripts/run-candidate.sh"), *args],
                              env=self.env, capture_output=True, text=True)

    def test_overrides_stable_environment_and_handles_spaces(self):
        self.fake_binary()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout.splitlines()[-1])
        data = self.root / ".smoke/candidate"
        self.assertEqual(config["CHATCMD_BIND"], "127.0.0.1")
        self.assertEqual(config["CHATCMD_PORT"], "8081")
        self.assertEqual(config["CHATCMD_DB_PATH"], str(data / "chatcmd.db"))
        self.assertEqual(config["CHATCMD_LOG_PATH"], str(data / "chatcmd.log"))
        self.assertEqual(config["cwd"], str(data))

    def test_missing_binary_does_not_create_data(self):
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse((self.root / ".smoke").exists())

    def test_rejects_linked_database(self):
        self.fake_binary()
        data = self.root / ".smoke/candidate"
        data.mkdir(parents=True)
        (data / "chatcmd.db").symlink_to(self.root / "stable.db")
        self.assertNotEqual(self.run_script().returncode, 0)

    def test_rejects_arguments(self):
        self.assertEqual(self.run_script("--release").returncode, 2)


if __name__ == "__main__":
    unittest.main()
