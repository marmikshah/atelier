#!/usr/bin/env python3
"""Build the static showcase into target/site using only the Python standard library."""

import argparse
import html
import json
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parent.parent
SHOWCASE = ROOT / "showcase"
OUTPUT = ROOT / "target" / "site"
PROVIDERS = {
    "Anthropic": ("claude", "Anthropic"),
    "OpenAI": ("codex", "OpenAI"),
    "Moonshot AI": ("kimi", "Moonshot AI"),
}


def escape(value):
    return html.escape(str(value), quote=True)


def icon(name):
    return f'<svg class="icon" aria-hidden="true"><use href="#icon-{name}"></use></svg>'


def model_name(model):
    name, _, effort = model.rpartition("-")
    if effort not in {"low", "medium", "high", "xhigh", "max"}:
        name, effort = model, ""
    if name.startswith("gpt-"):
        name = "GPT-" + name[4:].replace("-", " ").title()
    else:
        name = name.replace("-", " ").title()
    return name, effort


def load_data():
    data = json.loads((SHOWCASE / "runs.json").read_text())
    for key in ("models", "tasks"):
        values = data[key]
        if not values or len(set(values)) != len(values):
            raise ValueError(f"{key} must be non-empty and unique")
        if not all(re.fullmatch(r"[a-z0-9][a-z0-9.-]*", value) for value in values):
            raise ValueError(f"invalid {key} identifier")
    expected = {(model, task) for model in data["models"] for task in data["tasks"]}
    runs = {(run["model"], run["task"]): run for run in data["runs"]}
    if set(runs) != expected or len(runs) != len(data["runs"]):
        raise ValueError("runs must contain exactly the declared model/task matrix")
    for run in runs.values():
        stem = f'{run["model"]}/{run["task"]}'
        for key, directory, extension in (("gif", "gifs", "gif"), ("replay", "replays", "jsonl")):
            if run[key] != f"{directory}/{stem}.{extension}":
                raise ValueError(f"unexpected {key} path for {stem}")
            if not (SHOWCASE / run[key]).is_file():
                raise ValueError(f"missing {run[key]}")
        if run["vendor"] not in PROVIDERS:
            raise ValueError(f'add a provider group for {run["vendor"]}')
    for model in data["models"]:
        if len({runs[model, task]["vendor"] for task in data["tasks"]}) != 1:
            raise ValueError(f"inconsistent provider for {model}")
    return data, runs


def model_metadata(data, runs):
    # Interleave providers, newest recorded models first, so the first columns span providers.
    groups = [[model for model in reversed(data["models"])
               if runs[model, data["tasks"][0]]["vendor"] == vendor] for vendor in PROVIDERS]
    models = []
    for index in range(max(map(len, groups))):
        for group in groups:
            if index >= len(group):
                continue
            model = group[index]
            name, effort = model_name(model)
            vendor = runs[model, data["tasks"][0]]["vendor"]
            key, label = PROVIDERS[vendor]
            models.append({"id": model, "name": name, "effort": effort, "provider": key, "vendor": label})
    return models


def provider_badge(model):
    return f'<span class="provider-badge {model["provider"]}"><span></span>{escape(model["vendor"])}</span>'


def artwork(run, preview=False):
    name, effort = model_name(run["model"])
    display = f"{name} ({effort})" if effort else name
    canvas = 48 if run["task"] == "beam" else 32
    key = f'{run["model"]}/{run["task"]}'
    return f'''<a class="{'run-preview' if preview else 'artwork-stage'}" href="showcase/{escape(run['gif'])}"
      data-run="{key}" aria-label="Inspect {escape(run['task'].title())} by {escape(display)}" style="--canvas:{canvas}">
      <img src="showcase/{escape(run['gif'])}" width="{canvas * 4}" height="{canvas * 4}" loading="lazy"
        alt="{escape(run['task'].title())} by {escape(display)}">
      {'' if preview else f'<span class="inspect-mark">{icon("expand")}</span>'}</a>'''


def matrix_table(data, runs, models):
    headings = []
    for model in models:
        effort = f'<span class="effort">{model["effort"]}</span>' if model["effort"] else ""
        headings.append(f'''<th scope="col" data-model-column="{model['id']}"><div class="model-name">{escape(model['name'])}{effort}</div>{provider_badge(model)}</th>''')
    rows = []
    for index, task in enumerate(data["tasks"], start=1):
        cells = []
        for model in models:
            run = runs[model["id"], task]
            tokens = f'{run["tokens"]:,}' if run.get("tokens") is not None else "—"
            cells.append(f'''<td data-model-column="{model['id']}">{artwork(run)}
              <div class="cell-stats"><p><span><strong>{run['tool_calls']:,}</strong> calls</span><span><strong>{run['looks']:,}</strong> looks</span></p>
              <p><span>Tokens</span><strong title="{'Not reported' if tokens == '—' else 'Reported tokens'}">{tokens}</strong></p></div></td>''')
        rows.append(f'''<tr data-task-row="{task}"><th scope="row" class="brief-column"><span class="brief-number">{index:02}</span>
          <strong>{escape(task.title())}</strong><span class="canvas-size">{'48 × 48' if task == 'beam' else '32 × 32'} px</span>
          <a class="prompt-link" href="showcase/tasks/{task}.txt" data-brief="{task}">View prompt {icon('arrow')}</a></th>{''.join(cells)}</tr>''')
    return f'''<table id="matrix-table" class="matrix-table"><caption class="visually-hidden">Each row is one frozen brief. Each column is a model drawing that brief.</caption>
      <thead><tr><th scope="col" class="brief-column corner-heading">Brief <span>{len(data['tasks'])} tasks</span></th>{''.join(headings)}</tr></thead>
      <tbody>{''.join(rows)}</tbody></table>'''


def runs_table(data, runs, models):
    columns = [("task", "Brief"), ("model", "Model"), ("provider", "Provider"), (None, "Preview"),
               ("calls", "Calls"), ("looks", "Looks"), ("tokens", "Reported tokens"), (None, "Replay")]
    headings = []
    for key, label in columns:
        if key:
            headings.append(f'<th scope="col" data-sort-heading="{key}" aria-sort="{"ascending" if key == "task" else "none"}"><button type="button" data-sort="{key}">{label}{icon("sort")}</button></th>')
        else:
            headings.append(f'<th scope="col">{label}</th>')
    rows = []
    for task in data["tasks"]:
        for model in models:
            run = runs[model["id"], task]
            tokens = f'{run["tokens"]:,}' if run.get("tokens") is not None else "—"
            effort = f'<span class="effort">{model["effort"]}</span>' if model["effort"] else ""
            rows.append(f'''<tr data-run-row="{model['id']}/{task}"><th scope="row">{escape(task.title())}</th>
              <td><span class="data-model-name">{escape(model['name'])}</span>{effort}</td><td>{provider_badge(model)}</td>
              <td>{artwork(run, preview=True)}</td><td class="numeric">{run['tool_calls']:,}</td><td class="numeric">{run['looks']:,}</td>
              <td class="numeric" title="{'Not reported' if tokens == '—' else 'Reported tokens'}">{tokens}</td>
              <td><a class="replay-link" href="showcase/{escape(run['replay'])}" download aria-label="Download {escape(task)} replay by {escape(model['name'])}">{icon('download')}<span>Replay</span></a></td></tr>''')
    return f'''<table id="runs-table" class="runs-table" hidden><caption class="visually-hidden">Recorded run data. Counts vary between clients and are not an efficiency ranking.</caption>
      <thead><tr>{''.join(headings)}</tr></thead><tbody>{''.join(rows)}</tbody></table>'''


def filters(data, models):
    providers = []
    choices = []
    for vendor, (key, label) in PROVIDERS.items():
        count = sum(model["provider"] == key for model in models)
        if count:
            providers.append(f'<label class="check-option"><input type="checkbox" data-provider-filter value="{key}" checked><span class="provider-dot {key}"></span><span>{label}</span><span class="option-count">{count}</span></label>')
        for model in models:
            if model["provider"] != key:
                continue
            effort = f'<span class="model-effort">{model["effort"]}</span>' if model["effort"] else ""
            choices.append(f'<label class="check-option model-option" data-model-provider="{key}"><input type="checkbox" data-model-filter value="{model["id"]}" checked><span>{escape(model["name"])}{effort}</span></label>')
    return "".join(providers), "".join(choices)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    data, runs = load_data()
    models = model_metadata(data, runs)
    briefs = {task: (SHOWCASE / "tasks" / f"{task}.txt").read_text().strip() for task in data["tasks"]}
    provider_filters, model_filters = filters(data, models)
    client_data = {"models": models, "tasks": data["tasks"], "briefs": briefs, "runs": data["runs"]}
    replacements = {
        "MODEL_COUNT": str(len(models)), "RUN_COUNT": str(len(runs)), "TASK_COUNT": str(len(data["tasks"])),
        "TASK_OPTIONS": "".join(f'<option value="{task}">{escape(task.title())}</option>' for task in data["tasks"]),
        "PROVIDER_FILTERS": provider_filters, "MODEL_FILTERS": model_filters,
        "MATRIX_TABLE": matrix_table(data, runs, models), "RUNS_TABLE": runs_table(data, runs, models),
        "CLIENT_DATA": json.dumps(client_data, ensure_ascii=True, separators=(",", ":")).replace("<", "\\u003c"),
        "METHOD": escape(data["method"]), "SERVER": escape(data["server"]), "VERIFIED": escape(data["verified"]),
    }
    template = (ROOT / "site" / "index.html").read_text()
    for key, value in replacements.items():
        template = template.replace(f"@@{key}@@", value)
    if re.search(r"@@[A-Z_]+@@", template):
        raise ValueError("unresolved template placeholder")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for name in ("gifs", "replays", "tasks"):
        destination = OUTPUT / "showcase" / name
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(SHOWCASE / name, destination)
    shutil.copyfile(SHOWCASE / "runs.json", OUTPUT / "showcase" / "runs.json")
    for name in ("style.css", "gallery.js"):
        shutil.copyfile(ROOT / "site" / name, OUTPUT / name)
    (OUTPUT / "index.html").write_text(template)
    (OUTPUT / ".nojekyll").touch()
    print(f"Built {len(runs)} animations from {len(models)} models into target/site")


if __name__ == "__main__":
    main()
