#!/usr/bin/env python3
"""Migrate complete JSONL recipe collections to editable .atelier files.

Requires Python 3.11+. --out preserves inputs and directory names and writes a
verification report. Recipes with references export to directories.
--replace verifies the entire collection before deleting any legacy inputs.
Live documents are installed through Atelier's atomic store transaction; their
ids, revisions and checkpoints stay intact. External recipe filenames keep their
stems. Only the legacy importer reads JSONL; new recipes contain no command log.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def invoke(binary, *args):
    result = subprocess.run([binary, *map(str, args)], text=True, capture_output=True)
    if result.returncode:
        raise ValueError(f"Atelier failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def native_store(path):
    if path.name == "recipe.jsonl" and path.parent.parent.name == "documents":
        return path.parent.parent.parent
    if path.name == "recipe.jsonl" and path.parent.parent.name == ".checkpoints" and path.parent.parent.parent.parent.name == "documents":
        return path.parent.parent.parent.parent.parent
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path, help="legacy JSONL files or directories searched recursively")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--out", type=Path, help="new output directory; originals remain intact")
    mode.add_argument("--replace", action="store_true", help="install verified sources beside inputs and remove JSONL after all succeed")
    parser.add_argument("--atelier", default="atelier", help="Atelier executable")
    args = parser.parse_args()
    paths = {}
    for item in args.inputs:
        if item.is_symlink():
            parser.error(f"symlinks are refused: {item}")
        item=item.resolve()
        candidates = sorted(item.rglob("*.jsonl")) if item.is_dir() else [item]
        for path in candidates:
            if path.is_symlink() or not path.is_file() or path.suffix != ".jsonl":
                parser.error(f"expected a regular JSONL file: {path}")
            relative = path.relative_to(item) if item.is_dir() else Path(item.name)
            if len(args.inputs) > 1:
                prefix = item.parent.name if item.name == ".atelier" else item.name
                relative = Path(prefix) / relative
            paths[path] = relative
    if not paths:
        parser.error("no JSONL recipes found")
    if args.out:
        if args.out.exists():
            parser.error("--out must be new; outputs are never overwritten")
        args.out.mkdir(parents=True)
    # Verify input identities before performing any conversion or deletion.
    inputs = [(p, relative, digest(p)) for p, relative in sorted(paths.items())]
    records = []
    pending_delete = []
    stores = set()
    for path, relative, sha in inputs:
        store = native_store(path) if args.replace else None
        if store:
            stores.add(store)
            continue
        destination = (args.out / relative if args.out else path).with_suffix(".atelier")
        if destination.exists():
            raise ValueError(f"destination already exists: {destination}")
        if digest(path) != sha:
            raise ValueError(f"input changed: {path}")
        try:
            result = invoke(args.atelier, "migrate", path, destination)
        except ValueError as error:
            if "recipes with reference images need a directory destination" not in str(error):
                raise
            destination = destination.with_suffix("")
            result = invoke(args.atelier, "migrate", path, destination)
        if not result.get("pixels_equal") or not result.get("metadata_equal"):
            raise ValueError(f"migration did not verify exact pixels and metadata: {path}")
        if digest(path) != sha:
            raise ValueError(f"input changed during conversion: {path}")
        records.append({"original": str(path), "sha256": sha, **result})
        pending_delete.append((path, sha))
        print(f"{relative}: {result['manifest_bytes']:,} bytes, exact pixels verified", file=sys.stderr)
        if args.out:
            (args.out / "migration.json").write_text(json.dumps({"complete": False, "recipes": records}, indent=2) + "\n")
    for path, sha in pending_delete:
        if digest(path) != sha:
            raise ValueError(f"input changed; no legacy files deleted: {path}")
    for store in sorted(stores):
        result = invoke(args.atelier, "migrate", "--store", "--home", store)
        records.extend(result["documents"])
    if args.replace:
        for path, sha in pending_delete:
            if digest(path) != sha:
                raise ValueError(f"input changed before deletion: {path}")
            path.unlink()
    report = {"complete": True, "count": len(inputs), "recipes": records}
    if args.out:
        (args.out / "migration.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"complete": True, "recipes": len(inputs), "out": str(args.out) if args.out else None}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"migrate-recipes: {error}", file=sys.stderr)
        sys.exit(1)
