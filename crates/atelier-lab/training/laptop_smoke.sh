#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
RESEARCH_DIR="${RESEARCH_DIR:-${REPO_ROOT}/research}"
OUTPUT_DIR="${OUTPUT_DIR:-${RESEARCH_DIR}/checkpoints/generator-laptop-smoke}"
CONFIG="${SCRIPT_DIR}/configs/generator-qwen3.5-2b-qlora-15gb.json"
TASKS="${RESEARCH_DIR}/laptop-tasks.jsonl"
EPISODES="${RESEARCH_DIR}/laptop-episodes"
MANIFEST="${RESEARCH_DIR}/generator-episodes.jsonl"
DATASET_DIR="${RESEARCH_DIR}/generator-sft"
DRY_RUN=0

usage() {
  echo "usage: $0 [--dry-run]"
  echo
  echo "Environment overrides: PYTHON_BIN, RESEARCH_DIR, OUTPUT_DIR, HF_HOME"
}

while (($#)); do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

cd "${REPO_ROOT}"
mkdir -p "${RESEARCH_DIR}"
export HF_HOME="${HF_HOME:-${RESEARCH_DIR}/hf-cache}"

echo "==> Checking Python"
"${PYTHON_BIN}" --version

echo "==> Building atelier-lab"
cargo build --locked -p atelier-lab --bin atelier-lab
LAB="${REPO_ROOT}/target/debug/atelier-lab"

echo "==> Creating 8 development and 2 validation smoke tasks"
TASKS="${TASKS}" "${PYTHON_BIN}" - <<'PY'
import json
import os
from pathlib import Path

source = Path("crates/atelier-lab/tasks/development.jsonl")
destination = Path(os.environ["TASKS"])
rows = [json.loads(line) for line in source.read_text().splitlines() if line.strip()][:10]
if len(rows) != 10:
    raise SystemExit(f"expected at least 10 bundled tasks, found {len(rows)}")
with destination.open("w") as output:
    for index, row in enumerate(rows, 1):
        row["id"] = f"laptop-task-{index:03d}"
        row["split"] = "development" if index <= 8 else "validation"
        output.write(json.dumps(row, separators=(",", ":")) + "\n")
print(f"tasks={destination} rows={len(rows)}")
PY
"${LAB}" validate-tasks "${TASKS}"

echo "==> Recording deterministic offline episodes"
"${LAB}" run-batch "${TASKS}" "${EPISODES}" \
  --policy "${PYTHON_BIN}" \
  --policy-arg crates/atelier-lab/policy/example_policy.py \
  --name deterministic-example \
  --limit 10 \
  --repeats 1 \
  --max-turns 10

echo "==> Creating the curated episode manifest"
EPISODES="${EPISODES}" MANIFEST="${MANIFEST}" RESEARCH_DIR="${RESEARCH_DIR}" \
  "${PYTHON_BIN}" - <<'PY'
import json
import os
from pathlib import Path

episodes = Path(os.environ["EPISODES"])
manifest = Path(os.environ["MANIFEST"])
research = Path(os.environ["RESEARCH_DIR"])
records = [
    json.loads(line)
    for line in (episodes / "batch-results.jsonl").read_text().splitlines()
    if line.strip()
]
completed = {record["task_id"]: record for record in records if record["completed"]}
if len(completed) != 10:
    raise SystemExit(
        f"expected 10 completed smoke episodes, found {len(completed)}; "
        f"inspect {episodes / 'batch-results.jsonl'}"
    )
with manifest.open("w") as output:
    for task_id in sorted(completed):
        record = completed[task_id]
        episode = episodes / record["episode_dir"]
        row = {
            "format_version": 1,
            "id": f"{task_id}-seed-{record['seed']}",
            "episode_dir": episode.relative_to(research).as_posix(),
            "source": "model",
            "generator": record["policy"],
        }
        output.write(json.dumps(row, separators=(",", ":")) + "\n")
print(f"manifest={manifest} episodes={len(completed)}")
PY

echo "==> Exporting generator SFT rows"
"${LAB}" export-generator "${MANIFEST}" "${DATASET_DIR}"

echo "==> Validating the complete train/validation pipeline"
"${PYTHON_BIN}" "${SCRIPT_DIR}/quick_vlm.py" \
  generator \
  "${DATASET_DIR}/generator.jsonl" \
  "${DATASET_DIR}/artifacts" \
  "${CONFIG}" \
  --epochs 1 \
  --train-limit 32 \
  --val-limit 8 \
  --output-dir "${OUTPUT_DIR}" \
  --dry-run

if ((DRY_RUN)); then
  echo "PASS: laptop smoke data is ready; training was skipped"
  exit 0
fi

echo "==> Checking CUDA"
"${PYTHON_BIN}" - <<'PY'
import torch

print(f"torch={torch.__version__}")
if not torch.cuda.is_available():
    raise SystemExit(
        "CUDA is unavailable. Install a CUDA-enabled PyTorch build and retry."
    )
properties = torch.cuda.get_device_properties(0)
print(f"gpu={properties.name}")
print(f"vram={properties.total_memory / 1024**3:.1f}GiB")
PY

if [[ -d "${OUTPUT_DIR}" ]] && [[ -n "$(find "${OUTPUT_DIR}" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  echo "error: output directory is not empty: ${OUTPUT_DIR}" >&2
  echo "Set OUTPUT_DIR to a new path before retrying." >&2
  exit 1
fi

echo "==> Training and validating Qwen3.5-2B QLoRA"
"${PYTHON_BIN}" "${SCRIPT_DIR}/quick_vlm.py" \
  generator \
  "${DATASET_DIR}/generator.jsonl" \
  "${DATASET_DIR}/artifacts" \
  "${CONFIG}" \
  --epochs 1 \
  --train-limit 32 \
  --val-limit 8 \
  --output-dir "${OUTPUT_DIR}"

echo "PASS: adapter=${OUTPUT_DIR}"
