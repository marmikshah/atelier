"""Regression proofs for the portable verifier and its artifact contract."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from tools import showcase


class ShowcaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="atelier-tool-tests-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.matrix = self.root / 'art with spaces and "quotes"'
        shutil.copytree(Path(__file__).parent / "fixtures/showcase", self.matrix)

    def verify(self):
        return subprocess.run(
            [
                "python3",
                str(showcase.ROOT / "tools/showcase.py"),
                "verify",
                "--showcase",
                str(self.matrix),
                "--binary",
                str(showcase.ROOT / "target/debug/atelier"),
            ],
            cwd=self.root,
            capture_output=True,
            text=True,
        )

    def test_verifies_baseline_artwork_with_quoted_paths_from_another_directory(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("all 1 replays match", result.stdout)

    def test_artwork_mismatch_fails_without_replacing_the_recorded_artifact(self):
        gif = self.matrix / "gifs/fixture/tiny.gif"
        changed = gif.read_bytes() + b"changed"
        gif.write_bytes(changed)
        result = self.verify()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("replayed GIF differs", result.stderr)
        self.assertEqual(gif.read_bytes(), changed)

    def test_path_traversal_and_missing_artifact_pairs_are_rejected(self):
        manifest = self.matrix / "runs.json"
        original = manifest.read_text()
        data = json.loads(original)
        data["models"] = ["../outside"]
        manifest.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "valid identifiers"):
            showcase.showcase_matrix(self.matrix)
        manifest.write_text(original)
        (self.matrix / "replays/fixture/tiny.jsonl").unlink()
        with self.assertRaisesRegex(ValueError, "replays differs"):
            showcase.showcase_matrix(self.matrix)

    def test_collector_exports_replays_and_checks_resume_provenance(self):
        output = self.root / "collected"
        output.mkdir()
        wrapper = output / "atelier"
        wrapper.write_text(showcase.WRAPPER)
        schema = output / "schema.json"
        schema.write_text(json.dumps(showcase.SCHEMA))
        args = SimpleNamespace(
            output=output,
            model="fixture-model",
            effort="high",
            label="fixture",
            resume=False,
        )
        manifest = {"source": "offline workflow test"}
        actual_run = subprocess.run

        def codex_session(command, **kwargs):
            if command[0] != "codex":
                return actual_run(command, **kwargs)
            identity = None
            recipe = (
                Path(__file__).parent / "fixtures/showcase/replays/fixture/tiny.jsonl"
            )
            for line in recipe.read_text().splitlines():
                step = json.loads(line)
                params = step["args"]
                if step["tool"] == "doc_new":
                    params.pop("doc_id")
                else:
                    params["doc_id"] = identity
                result = actual_run(
                    [
                        sys.executable,
                        str(wrapper),
                        "call",
                        step["tool"],
                        json.dumps(params),
                    ],
                    env=kwargs["env"],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                identity = json.loads(result.stdout)["doc_id"]
            result_path = Path(command[command.index("--output-last-message") + 1])
            result_path.write_text(
                json.dumps({"doc_id": identity, "summary": "Tiny animation"})
            )
            kwargs["stdout"].write(
                json.dumps({"type": "thread.started", "thread_id": "fixture"}) + "\n"
            )
            return subprocess.CompletedProcess(command, 0)

        with (
            patch.object(showcase.subprocess, "run", side_effect=codex_session),
            patch.object(
                showcase,
                "session_usage",
                return_value=(None, [{"model": args.model, "effort": args.effort}]),
            ),
        ):
            result = showcase.run_task(
                "alien",
                args,
                manifest,
                showcase.ROOT / "target/debug/atelier",
                wrapper,
                schema,
            )
        self.assertEqual(result["tool_calls"], 4)
        self.assertEqual(result["provenance"], manifest)
        args.resume = True
        self.assertEqual(
            showcase.run_task(
                "alien",
                args,
                manifest,
                showcase.ROOT / "target/debug/atelier",
                wrapper,
                schema,
            ),
            result,
        )
        with self.assertRaisesRegex(RuntimeError, "different provenance"):
            showcase.run_task(
                "alien",
                args,
                {},
                showcase.ROOT / "target/debug/atelier",
                wrapper,
                schema,
            )


if __name__ == "__main__":
    unittest.main()
