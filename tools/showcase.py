#!/usr/bin/env python3
"""Verify recorded showcase artwork or collect isolated Codex runs.

Build Atelier first with cargo build --locked -p atelier. Verification is
offline; collecting new artwork requires an authenticated Codex CLI.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
WRAPPER = """#!/usr/bin/env python3
import json
import os
import sys

args = sys.argv[1:]
if not args or args[0] not in ("call", "tools", "skills", "--version"):
    sys.exit("Use call, tools, or skills for benchmark artwork.")
if args[0] == "call":
    if "--home" in args:
        sys.exit("The benchmark wrapper supplies the isolated home.")
    with open(os.environ["ATELIER_BENCHMARK_LOG"], "a") as log:
        log.write(json.dumps({"tool": args[1] if len(args) > 1 else None,
                              "args": args[2:]}) + "\\n")
    args += ["--home", os.environ["ATELIER_BENCHMARK_HOME"]]
os.execv(os.environ["ATELIER_BENCHMARK_BINARY"], ["atelier", *args])
"""
SCHEMA = {
    "type": "object",
    "properties": {
        "doc_id": {"type": "string"},
        "summary": {"type": "string"},
    },
    "required": ["doc_id", "summary"],
    "additionalProperties": False,
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    temporary = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def atelier_call(binary, home, tool, args):
    result = subprocess.run(
        [str(binary), "call", tool, json.dumps(args), "--home", str(home)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def replay_gif(binary, recipe, home, gif, log_path):
    """Replay one recipe and export its sole document at the showcase scale."""
    with log_path.open("w") as log:
        subprocess.run(
            [str(binary), "replay", str(recipe), "--home", str(home)],
            check=True,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
    documents = atelier_call(binary, home, "list_docs", {})["documents"]
    if len(documents) != 1:
        raise RuntimeError(
            f"{recipe}: replay created {len(documents)} documents, expected one"
        )
    atelier_call(
        binary,
        home,
        "doc_export",
        {
            "doc_id": documents[0]["doc_id"],
            "op": "anim",
            "format": "gif",
            "scale": 4,
            "out_path": str(gif),
        },
    )


def showcase_matrix(root):
    """Require the declared matrix and its original GIF/recipe pairs."""
    data = json.loads((root / "runs.json").read_text())
    for key in ["models", "tasks"]:
        values = data[key]
        if (
            not values
            or any(
                not isinstance(value, str)
                or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", value)
                for value in values
            )
            or len(set(values)) != len(values)
        ):
            raise ValueError(f"{key} must contain unique, non-empty, valid identifiers")
    expected = {(model, task) for model in data["models"] for task in data["tasks"]}
    runs = [(run["model"], run["task"]) for run in data["runs"]]
    if len(runs) != len(expected) or set(runs) != expected:
        raise ValueError(
            "runs.json must contain exactly the declared model/task matrix"
        )
    for run in data["runs"]:
        stem = f"{run['model']}/{run['task']}"
        if run["replay"] != f"replays/{stem}.jsonl" or run["gif"] != f"gifs/{stem}.gif":
            raise ValueError(f"incorrect artifact paths for {stem}")
    for directory, extension in [("replays", "jsonl"), ("gifs", "gif")]:
        actual = {
            str(path.relative_to(root / directory))
            for path in (root / directory).rglob(f"*.{extension}")
        }
        required = {f"{model}/{task}.{extension}" for model, task in expected}
        if actual != required:
            raise ValueError(f"{directory} differs from the declared model/task matrix")
    return data


def verify(args):
    data = showcase_matrix(args.showcase)
    with tempfile.TemporaryDirectory(prefix="atelier-showcase-") as temporary:
        root = Path(temporary)
        for index, run in enumerate(data["runs"], 1):
            stem = f"{run['model']}/{run['task']}"
            directory = root / stem
            directory.mkdir(parents=True)
            log = directory / "replay.log"
            actual = directory / "replayed.gif"
            try:
                replay_gif(
                    args.binary,
                    args.showcase / run["replay"],
                    directory / "home",
                    actual,
                    log,
                )
                if actual.read_bytes() != (args.showcase / run["gif"]).read_bytes():
                    raise RuntimeError(
                        "replayed GIF differs from the committed artwork"
                    )
            except (
                OSError,
                ValueError,
                subprocess.CalledProcessError,
                RuntimeError,
            ) as error:
                if log.exists():
                    print(log.read_text(), file=sys.stderr)
                raise RuntimeError(f"{stem}: {error}") from error
            print(f"verify: {index}/{len(data['runs'])} {stem} matches", flush=True)
    print(f"verify: all {len(data['runs'])} replays match their committed GIFs")
    return 0


def session_usage(thread_id):
    """Read cumulative usage for this session, including interrupted turns."""
    sessions = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "sessions"
    usage = None
    settings = []
    for path in sessions.rglob(f"*{thread_id}*.jsonl"):
        for line in path.read_text().splitlines():
            event = json.loads(line)
            payload = event.get("payload", {})
            if event.get("type") == "turn_context":
                settings.append(
                    {"model": payload.get("model"), "effort": payload.get("effort")}
                )
            if (
                event.get("type") == "event_msg"
                and payload.get("type") == "token_count"
            ):
                info = payload.get("info") or {}
                usage = info.get("total_token_usage") or usage
    return usage, settings


def run_task(task, args, manifest, binary, wrapper, schema):
    directory = args.output / task
    complete = directory / "complete.json"
    if args.resume and complete.is_file():
        result = json.loads(complete.read_text())
        if result["provenance"] != manifest:
            raise RuntimeError(f"{task}: completed run has different provenance")
        if (
            digest(directory / f"{task}.gif") != result["gif_sha256"]
            or digest(directory / f"{task}.jsonl") != result["replay_sha256"]
        ):
            raise RuntimeError(f"{task}: completed artifacts changed")
        print(f"{task}: retaining completed run", flush=True)
        return result
    continuing = directory.exists()
    if continuing and not args.resume:
        raise RuntimeError(
            f"{task}: partial run exists; use --resume to continue its session"
        )
    directory.mkdir(exist_ok=True)
    home = directory / "home"
    home.mkdir(exist_ok=True)
    brief = ROOT / "showcase" / "tasks" / f"{task}.txt"
    prompt = f"""Create the following animated pixel-art sprite using Atelier.

{brief.read_text().strip()}

Load and follow the atelier-sprite skill by reading
{ROOT / "crates/atelier/skills/sprite.md"}.
Use {wrapper} for every Atelier invocation. It supplies this task's isolated
store and records calls. Inspect tool schemas with its tools command as needed.
Create all artwork through Atelier's editing tools. Shell/Python can construct
arguments and loops that call those tools. Use doc_look with --image-out and
your image viewer to inspect the actual images, and follow the skill's drawing
and animation workflow. Save previews and temporary argument files here.

This is an independent benchmark run. Work only on this brief. Do not inspect
other models' art or replay recipes. Do not edit repository source, the frozen
brief, or store files directly, and do not use git or GitHub commands. Do not
read credentials. Finish with exactly one document containing the requested
animation. Return its doc_id and a short summary. The harness exports the final
GIF and verifies its replay; you may export to inspect your own animation.
"""
    if not continuing:
        (directory / "prompt.txt").write_text(prompt)
    environment = dict(os.environ)
    environment.update(
        {
            "ATELIER_BENCHMARK_HOME": str(home),
            "ATELIER_BENCHMARK_BINARY": str(binary),
            "ATELIER_BENCHMARK_LOG": str(directory / "calls.jsonl"),
        }
    )
    command = [
        "codex",
        "exec",
        "--ignore-user-config",
        "--model",
        args.model,
        "-c",
        f'model_reasoning_effort="{args.effort}"',
        "-c",
        'approval_policy="never"',
        "--sandbox",
        "workspace-write",
        "--json",
        "--output-schema",
        str(schema),
        "--output-last-message",
        str(directory / "result.json"),
        "--cd",
        str(directory),
        "-",
    ]
    if continuing:
        events = [
            json.loads(line)
            for line in (directory / "events.jsonl").read_text().splitlines()
        ]
        thread_id = next(
            event["thread_id"]
            for event in events
            if event.get("type") == "thread.started"
        )
        command = [
            "codex",
            "exec",
            "resume",
            "--ignore-user-config",
            "--model",
            args.model,
            "-c",
            f'model_reasoning_effort="{args.effort}"',
            "-c",
            'approval_policy="never"',
            "-c",
            'sandbox_mode="workspace-write"',
            "--json",
            "--output-schema",
            str(schema),
            "--output-last-message",
            str(directory / "result.json"),
            thread_id,
            "-",
        ]
        prompt = (
            "Continue your original showcase task in the same isolated store. "
            "The previous CLI turn was interrupted; preserve its artwork and follow "
            "the original brief and tool-only workflow. Complete any remaining checks "
            "and return the final doc_id and summary in the required JSON format."
        )
    with (directory / "invocations.jsonl").open("a") as invocations:
        invocations.write(json.dumps({"command": command, **manifest}) + "\n")
    print(
        f"{task}: {'resuming' if continuing else 'starting'} {args.model} / {args.effort}",
        flush=True,
    )
    with (directory / "events.jsonl").open("a") as events:
        with (directory / "stderr.log").open("a") as errors:
            process = subprocess.run(
                command,
                input=prompt,
                text=True,
                env=environment,
                stdout=events,
                stderr=errors,
                cwd=directory,
            )
    if process.returncode:
        raise RuntimeError(
            f"{task}: Codex exited {process.returncode}; see {directory}"
        )
    result = json.loads((directory / "result.json").read_text())
    documents = atelier_call(binary, home, "list_docs", {})["documents"]
    if len(documents) != 1 or documents[0]["doc_id"] != result["doc_id"]:
        raise RuntimeError(f"{task}: expected exactly the reported document")
    info = atelier_call(binary, home, "doc_info", {"doc_id": result["doc_id"]})
    source = home / "documents" / result["doc_id"] / "recipe.jsonl"
    recipe = directory / f"{task}.jsonl"
    shutil.copyfile(source, recipe)
    gif = directory / f"{task}.gif"
    atelier_call(
        binary,
        home,
        "doc_export",
        {
            "doc_id": result["doc_id"],
            "op": "anim",
            "format": "gif",
            "scale": 4,
            "out_path": str(gif),
        },
    )
    replay_home = directory / "replayed"
    replay_gif_path = directory / "replayed.gif"
    replay_gif(binary, recipe, replay_home, replay_gif_path, directory / "replay.log")
    if gif.read_bytes() != replay_gif_path.read_bytes():
        raise RuntimeError(f"{task}: replay changes the exported GIF")
    calls = [
        json.loads(line)
        for line in (directory / "calls.jsonl").read_text().splitlines()
    ]
    usage = None
    thread_id = None
    for line in (directory / "events.jsonl").read_text().splitlines():
        event = json.loads(line)
        if event.get("type") == "thread.started":
            thread_id = event.get("thread_id")
        if event.get("type") == "turn.completed":
            usage = event.get("usage")
    cumulative_usage, settings = session_usage(thread_id)
    if cumulative_usage:
        usage = cumulative_usage
    if not settings or any(
        setting != {"model": args.model, "effort": args.effort} for setting in settings
    ):
        raise RuntimeError(
            f"{task}: session did not confirm the requested model and reasoning effort"
        )
    record = {
        "task": task,
        "model": args.label,
        "vendor": "OpenAI",
        "model_id": args.model,
        "reasoning_effort": args.effort,
        "doc_id": result["doc_id"],
        "summary": result["summary"],
        "tool_calls": len(calls),
        "looks": sum(call["tool"] == "doc_look" for call in calls),
        "tokens": usage["input_tokens"] + usage["output_tokens"] if usage else None,
        "usage": usage,
        "thread_id": thread_id,
        "settings": settings,
        "document": info,
        "gif_sha256": digest(gif),
        "replay_sha256": digest(recipe),
        "provenance": manifest,
    }
    write_json(complete, record)
    print(f"{task}: complete, {len(calls)} calls, replay matches", flush=True)
    return record


def collect(args, parser):
    args.label = args.label or f"{args.model}-{args.effort}"
    if not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", args.label):
        parser.error(
            "label must contain only lowercase letters, digits, dots, and hyphens"
        )
    if args.jobs < 1:
        parser.error("jobs must be positive")
    frozen = showcase_matrix(ROOT / "showcase")["tasks"]
    tasks = args.tasks or frozen
    if len(set(tasks)) != len(tasks) or set(tasks) - set(frozen):
        parser.error("tasks must be unique names from showcase/runs.json")
    args.output = (args.output or ROOT / "target/showcase" / args.label).resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {
        "model_id": args.model,
        "reasoning_effort": args.effort,
        "atelier_commit": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip(),
        "atelier_binary_sha256": digest(args.binary),
        "atelier_version": subprocess.check_output(
            [str(args.binary), "--version"], text=True
        ).strip(),
        "codex_version": subprocess.check_output(
            ["codex", "--version"], text=True
        ).strip(),
        "skill_sha256": digest(ROOT / "crates/atelier/skills/sprite.md"),
        "brief_sha256": {
            task: digest(ROOT / "showcase/tasks" / f"{task}.txt") for task in frozen
        },
    }
    manifest_path = args.output / "manifest.json"
    if manifest_path.exists():
        if not args.resume or json.loads(manifest_path.read_text()) != manifest:
            parser.error(
                "output already contains a run; use a fresh path or --resume with identical provenance"
            )
    if not manifest_path.exists():
        write_json(manifest_path, manifest)
    binary = args.output / "atelier-binary"
    if not binary.exists():
        shutil.copy2(args.binary, binary)
    if digest(binary) != manifest["atelier_binary_sha256"]:
        parser.error("the pinned binary differs from the run manifest")
    wrapper = args.output / "atelier"
    if wrapper.exists():
        if wrapper.read_text() != WRAPPER:
            parser.error("the benchmark wrapper differs from this runner")
    else:
        wrapper.write_text(WRAPPER)
        wrapper.chmod(0o755)
    schema = args.output / "result-schema.json"
    if schema.exists():
        if json.loads(schema.read_text()) != SCHEMA:
            parser.error("the output schema differs from this runner")
    else:
        write_json(schema, SCHEMA)
    failures = []
    with ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {
            executor.submit(
                run_task, task, args, manifest, binary, wrapper, schema
            ): task
            for task in tasks
        }
        for future in as_completed(futures):
            try:
                future.result()
            except Exception as error:
                failures.append({"task": futures[future], "error": str(error)})
                print(str(error), file=sys.stderr, flush=True)
    records = [
        json.loads(path.read_text())
        for task in frozen
        if (path := args.output / task / "complete.json").is_file()
    ]
    write_json(args.output / "results.json", records)
    write_json(args.output / "failures.json", failures)
    return bool(failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    verification = commands.add_parser(
        "verify", help="Replay the matrix and compare GIF bytes"
    )
    verification.add_argument(
        "--showcase",
        type=Path,
        default=ROOT / "showcase",
        help="Matrix directory (default: repository showcase)",
    )
    run = commands.add_parser(
        "run", help="Collect new artwork with an authenticated Codex CLI"
    )
    run.add_argument("--model", required=True, help="Codex model id")
    run.add_argument(
        "--effort", required=True, choices=["low", "medium", "high", "xhigh", "max"]
    )
    run.add_argument("--label", help="Showcase model label; defaults to MODEL-EFFORT")
    run.add_argument(
        "--jobs", type=int, default=3, help="Concurrent sessions (default: 3)"
    )
    run.add_argument("--tasks", nargs="+", help="Subset of the frozen task names")
    run.add_argument("--output", type=Path, help="Ignored artifact directory")
    run.add_argument(
        "--resume",
        action="store_true",
        help="Retain completed tasks and continue interrupted sessions",
    )
    for command in [verification, run]:
        command.add_argument(
            "--binary",
            type=Path,
            default=Path(os.environ.get("ATELIER_BIN", ROOT / "target/debug/atelier")),
            help="Atelier executable (default: ATELIER_BIN or target/debug/atelier)",
        )
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error("build Atelier first: cargo build --locked -p atelier")
    try:
        if args.command == "verify":
            args.showcase = args.showcase.resolve()
            return verify(args)
        return collect(args, run)
    except (
        OSError,
        KeyError,
        ValueError,
        RuntimeError,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"showcase: {error}\n")


if __name__ == "__main__":
    sys.exit(main())
