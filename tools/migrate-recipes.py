#!/usr/bin/env python3
"""Migrate a recipe collection without changing its inputs or game paths.

Each verified bundle is published by Atelier. This script groups exact duplicate
command streams (ignoring working UUID stamps), chooses readable stable output
names, and writes an explicit original-to-source map. Similar artwork is never
deduplicated or inferred to be linked. Requires Python 3.10+ and a new Atelier.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import uuid


def read_recipe(path):
    with path.open("rb") as source:
        data = source.read(64 * 1024 * 1024 + 1)
    if len(data) > 64 * 1024 * 1024:
        raise ValueError(f"{path}: recipe exceeds 64 MiB")
    return data


def digest(path):
    return hashlib.sha256(read_recipe(path)).hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field {key!r}")
        result[key] = value
    return result


def identify(path):
    data = read_recipe(path)
    entries = []
    for number, line in enumerate(data.decode("utf-8").splitlines(), 1):
        if not line.strip():
            continue
        if len(entries) >= 100_000:
            raise ValueError(f"{path}: recipe exceeds 100,000 entries")
        try:
            entries.append(json.loads(line, object_pairs_hook=unique_object))
        except ValueError as error:
            raise ValueError(f"{path}, line {number}: {error}") from error
    if not entries or any(not isinstance(e, dict) or not isinstance(e.get("args"), dict) for e in entries) or entries[0].get("tool") != "doc_new":
        raise ValueError(f"{path}: expected a complete JSONL journal beginning with doc_new")
    if "source" in entries[0].get("args", {}):
        raise ValueError(f"{path}: source-backed journal; migrate it individually with atelier source migrate")
    original = entries[0]["args"].get("doc_id")
    if not isinstance(original, str):
        raise ValueError(f"{path}: doc_new needs a canonical UUIDv4 stamp")
    try:
        parsed = uuid.UUID(original)
    except ValueError as error:
        raise ValueError(f"{path}: invalid UUID stamp {original!r}") from error
    if parsed.version != 4 or str(parsed) != original:
        raise ValueError(f"{path}: doc_new needs a canonical UUIDv4 stamp")
    normalized = []
    for step in entries:
        step = dict(step)
        args = dict(step["args"])
        for key in ("doc_id", "set_doc"):
            if key in args:
                if args[key] != original:
                    raise ValueError(f"{path}: journal targets more than one document")
                args[key] = "document"
        step["args"] = args
        step.setdefault("format_version", 1)
        normalized.append(step)
    identity = hashlib.sha256(json.dumps(normalized, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()
    name = entries[0]["args"].get("name", path.stem)
    if not isinstance(name, str):
        raise ValueError(f"{path}: document name must be text")
    slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:64].rstrip("-") or "artwork"
    return hashlib.sha256(data).hexdigest(), identity, f"{slug}-{identity[:12]}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path, help="JSONL files or directories searched recursively")
    parser.add_argument("--out", required=True, type=Path, help="new output directory for bundles and migration.json")
    parser.add_argument("--atelier", default="atelier", help="Atelier executable (default: atelier on PATH)")
    parser.add_argument("--mode", choices=("auto", "pixels", "procedural"), default="auto")
    args = parser.parse_args()
    if args.out.exists():
        parser.error("--out must not exist; originals and previous migrations are never overwritten")
    paths = set()
    for item in args.inputs:
        if item.is_dir():
            paths.update(p.resolve() for p in item.rglob("*.jsonl"))
        else:
            paths.add(item.resolve())
    if not paths:
        parser.error("no JSONL recipes found")
    # Check the complete input list before starting any migration.
    inputs = [(p, *identify(p)) for p in sorted(paths)]
    args.out.mkdir(parents=True)
    report = {"format": 1, "complete": False, "recipes": [], "bundles": {}}
    record = args.out / "migration.json"

    def save_report():
        temporary = record.with_suffix(".tmp")
        temporary.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        temporary.replace(record)

    save_report()
    for path, source_hash, identity, name in inputs:
        if digest(path) != source_hash:
            raise ValueError(f"{path}: changed during migration; stopped")
        if identity not in report["bundles"]:
            destination = args.out / name
            completed = subprocess.run([args.atelier, "source", "migrate", str(path), str(destination), "--mode", args.mode], text=True, capture_output=True)
            if completed.returncode:
                report["error"] = {"input": str(path), "message": completed.stderr.strip()}
                save_report()
                raise ValueError(f"migration failed for {path}: {completed.stderr.strip()}")
            result = json.loads(completed.stdout)
            if digest(path) != source_hash:
                raise ValueError(f"{path}: changed while Atelier replayed it; stopped")
            result["source"] = f"{name}/source.toml"
            report["bundles"][identity] = result
            print(f"{name}: {result['manifest_bytes'] + result['resource_bytes']:,} bytes, pixels verified", file=sys.stderr)
        report["recipes"].append({"original": str(path), "sha256": source_hash, "source": report["bundles"][identity]["source"]})
        save_report()
    report["complete"] = True
    save_report()
    print(json.dumps({"mapping": str(record), "recipes": len(inputs), "distinct_sources": len(report["bundles"])}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"migrate-recipes: {error}", file=sys.stderr)
        sys.exit(1)
