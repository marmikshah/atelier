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
    "Anthropic": ("claude", "Claude"),
    "OpenAI": ("codex", "Codex"),
    "Moonshot AI": ("kimi", "Kimi"),
}


def escape(value):
    return html.escape(str(value), quote=True)


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


def card(run):
    name, effort = model_name(run["model"])
    badge = f'<span class="effort">{escape(effort)}</span>' if effort else ""
    tokens = f'{run["tokens"]:,}' if run.get("tokens") is not None else "Unavailable"
    size = 192 if run["task"] == "beam" else 128
    return f"""<article class="card">
      <div class="card-heading"><h4>{escape(name)}</h4>{badge}</div>
      <a class="stage" href="showcase/{escape(run['gif'])}" aria-label="Open {escape(name)} {escape(run['task'])} GIF">
        <img src="showcase/{escape(run['gif'])}" width="{size}" height="{size}" loading="lazy"
          alt="{escape(run['task'].title())} animation by {escape(name)}" class="sprite" style="--sprite-size:{size}px">
        <span class="stage-caption">{run['frames']} frames · 1 sec loop</span>
      </a>
      <div class="card-footer"><p><strong>{run['tool_calls']:,}</strong> calls <span>·</span> <strong>{run['looks']:,}</strong> looks</p>
        <a href="showcase/{escape(run['replay'])}" download>Replay <span aria-hidden="true">↗</span></a></div>
      <details class="run-details"><summary>Run details</summary>
        <dl><div><dt>Recorded model</dt><dd>{escape(run['model'])}</dd></div>
          <div><dt>Reported tokens</dt><dd>{tokens}</dd></div></dl>
        <p>Counts vary by client. Token totals can include cached input. See the method below.</p>
      </details>
    </article>"""


def gallery(data, runs):
    sections = []
    for task in data["tasks"]:
        brief = (SHOWCASE / "tasks" / f"{task}.txt").read_text().strip()
        groups = []
        for vendor, (key, label) in PROVIDERS.items():
            models = [model for model in reversed(data["models"]) if runs[model, task]["vendor"] == vendor]
            if not models:
                continue
            cards = "\n".join(card(runs[model, task]) for model in models)
            groups.append(f"""<section class="provider-group" data-provider="{key}">
              <div class="provider-heading"><h3><span class="provider-dot {key}"></span>{label}</h3>
                <p>{escape(vendor)} <span> / </span> {len(models)} {'model' if len(models) == 1 else 'models'}</p></div>
              <div class="cards">{cards}</div></section>""")
        sections.append(f"""<section class="task-section" data-task="{task}" aria-labelledby="task-{task}">
          <div class="task-heading"><div><span class="eyebrow">The brief</span><h2 id="task-{task}">{escape(task.title())}</h2></div>
            <span class="canvas-label">{'48 × 48' if task == 'beam' else '32 × 32'} px / 10 fps</span></div>
          <details class="brief"><summary>Read the original prompt</summary><p>{escape(brief)}</p></details>
          {''.join(groups)}</section>""")
    return "\n".join(sections)


def hero_samples(data, runs):
    samples = []
    for index, (vendor, (_, label)) in enumerate(PROVIDERS.items()):
        models = [model for model in data["models"] if runs[model, data["tasks"][0]]["vendor"] == vendor]
        if not models:
            continue
        task = ("alien", "potion", "cat")[index]
        if task not in data["tasks"]:
            task = data["tasks"][0]
        run = runs[models[-1], task]
        name, _ = model_name(run["model"])
        samples.append(f'<div class="hero-sample sample-{index + 1}"><img src="showcase/{escape(run["gif"])}" '
                       f'width="128" height="128" alt="{escape(name)} {escape(task)} animation">'
                       f'<span>{label} / {escape(task.title())}</span></div>')
    return "".join(samples)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    data, runs = load_data()
    vendors = {run["vendor"] for run in data["runs"]}
    template = (ROOT / "site" / "index.html").read_text()
    replacements = {
        "MODEL_COUNT": str(len(data["models"])),
        "RUN_COUNT": str(len(runs)),
        "PROVIDER_COUNT": str(len(vendors)),
        "TASK_COUNT": str(len(data["tasks"])),
        "TASK_OPTIONS": "".join(f'<option value="{task}">{escape(task.title())}</option>' for task in data["tasks"]),
        "PROVIDER_BUTTONS": "".join(f'<button type="button" data-filter="{key}" aria-pressed="false">{label}</button>'
                                    for vendor, (key, label) in PROVIDERS.items() if vendor in vendors),
        "GALLERY": gallery(data, runs),
        "HERO_SAMPLES": hero_samples(data, runs),
        "METHOD": escape(data["method"]),
        "SERVER": escape(data["server"]),
        "VERIFIED": escape(data["verified"]),
    }
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
    print(f"Built {len(runs)} animations from {len(data['models'])} models into target/site")


if __name__ == "__main__":
    main()
