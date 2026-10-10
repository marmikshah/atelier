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

    def draw(self, wrapper, environment):
        """Draw the fixture recipe through the benchmark wrapper, as an agent would."""
        identity = None
        recipe = Path(__file__).parent / "fixtures/showcase/replays/fixture/tiny.jsonl"
        for line in recipe.read_text().splitlines():
            step = json.loads(line)
            params = step["args"]
            if step["tool"] == "doc_new":
                params.pop("doc_id")
            else:
                params["doc_id"] = identity
            result = subprocess.run(
                [
                    sys.executable,
                    str(wrapper),
                    "call",
                    step["tool"],
                    json.dumps(params),
                ],
                env=environment,
                check=True,
                capture_output=True,
                text=True,
            )
            identity = json.loads(result.stdout)["doc_id"]
        return identity

    def test_claude_collector_reads_results_usage_and_models_from_the_stream(self):
        output = self.root / "collected"
        output.mkdir()
        wrapper = output / "atelier"
        wrapper.write_text(showcase.WRAPPER)
        schema = output / "schema.json"
        schema.write_text(json.dumps(showcase.SCHEMA))
        args = SimpleNamespace(
            output=output,
            client="claude",
            model="fixture-model",
            effort="xhigh",
            label="fixture",
            resume=True,
        )
        binary = showcase.ROOT / "target/debug/atelier"
        actual_run = subprocess.run
        sessions = []

        def turn(identifier, model, tokens):
            usage = {"input_tokens": 1, "cache_read_input_tokens": tokens}
            message = {"id": identifier, "model": model, "usage": usage}
            return {"type": "assistant", "message": message}

        def claude_session(killed, model="fixture-model"):
            def run(command, **kwargs):
                if command[0] != "claude":
                    return actual_run(command, **kwargs)
                sessions.append(command)
                events = [
                    {"type": "system", "subtype": "init", "session_id": "fixture"}
                ]
                if killed:
                    # A killed invocation leaves its turns but no result or usage summary.
                    events.append(turn("first", model, 1000))
                    kwargs["stdout"].write(
                        "".join(json.dumps(e) + "\n" for e in events)
                    )
                    return subprocess.CompletedProcess(command, 137)
                identity = self.draw(wrapper, kwargs["env"])
                events.append(turn("second", model, 500))
                reported = {
                    "inputTokens": 1,
                    "cacheReadInputTokens": 500,
                    "outputTokens": 7,
                }
                events.append(
                    {
                        "type": "result",
                        "is_error": False,
                        "structured_output": {"doc_id": identity, "summary": "Tiny"},
                        "modelUsage": {model: reported},
                    }
                )
                kwargs["stdout"].write("".join(json.dumps(e) + "\n" for e in events))
                return subprocess.CompletedProcess(command, 0)

            return patch.object(showcase.subprocess, "run", side_effect=run)

        with claude_session(killed=True):
            with self.assertRaisesRegex(RuntimeError, "claude exited 137"):
                showcase.run_task("alien", args, {}, binary, wrapper, schema)
        with claude_session(killed=False):
            result = showcase.run_task("alien", args, {}, binary, wrapper, schema)
        self.assertNotIn("--resume", sessions[0])
        self.assertEqual(sessions[1][-2:], ["--resume", "fixture"])
        self.assertEqual(result["vendor"], "Anthropic")
        self.assertEqual(result["tool_calls"], 4)
        self.assertEqual(result["settings"], [{"model": args.model, "effort": "xhigh"}])
        # The killed turn's input comes from its message; its output was never reported.
        self.assertEqual(result["usage"]["cached_input_tokens"], 1500)
        self.assertEqual(result["tokens"], 1502 + 7)
        self.assertFalse(result["usage"]["complete"])

        args.output = self.root / "fallback"
        args.output.mkdir()
        with claude_session(killed=False, model="another-model"):
            with self.assertRaisesRegex(
                RuntimeError, "did not confirm the requested model"
            ):
                showcase.run_task("alien", args, {}, binary, wrapper, schema)

    def test_collector_exports_replays_and_checks_resume_provenance(self):
        output = self.root / "collected"
        output.mkdir()
        wrapper = output / "atelier"
        wrapper.write_text(showcase.WRAPPER)
        schema = output / "schema.json"
        schema.write_text(json.dumps(showcase.SCHEMA))
        args = SimpleNamespace(
            output=output,
            client="codex",
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
            identity = self.draw(wrapper, kwargs["env"])
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
