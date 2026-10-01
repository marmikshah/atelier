"use strict";

const data = JSON.parse(document.querySelector("#showcase-data").textContent);
const modelIds = data.models.map(model => model.id);
const providerIds = [...new Set(data.models.map(model => model.provider))];
const models = new Map(data.models.map(model => [model.id, model]));
const runKey = run => `${run.model}/${run.task}`;
const runs = new Map(data.runs.map(run => [runKey(run), run]));
const sourceRuns = data.tasks.flatMap(task => modelIds.map(model => runs.get(`${model}/${task}`)));
const matrix = document.querySelector("#matrix-table");
const runTable = document.querySelector("#runs-table");
const matrixRows = [...matrix.rows].map(row => ({row,
  cells: new Map([...row.querySelectorAll("[data-model-column]")].map(cell => [cell.dataset.modelColumn, cell]))}));
const runRows = new Map([...document.querySelectorAll("[data-run-row]")].map(row => [row.dataset.runRow, row]));
const providerInputs = [...document.querySelectorAll("[data-provider-filter]")];
const modelInputs = [...document.querySelectorAll("[data-model-filter]")];
const taskSelect = document.querySelector("#task-select");
const search = document.querySelector("#brief-search");
const zoomSelect = document.querySelector("#pixel-zoom");
const statsInput = document.querySelector("#show-stats");
const scrollPanel = document.querySelector(".table-scroll");
const shareButton = document.querySelector("#share-link");
const shareStatus = document.querySelector("#share-status");
const copyFallback = document.querySelector(".copy-fallback");
const filterPanel = document.querySelector("#filter-panel");
const filterHost = document.querySelector(".filter-host");
const filterDialog = document.querySelector("#filter-dialog");
const runDialog = document.querySelector("#run-dialog");
const briefDialog = document.querySelector("#brief-dialog");
const sortKeys = ["task", "model", "provider", "calls", "looks", "tokens"];
const number = value => value == null ? "Not reported" : value.toLocaleString("en-US");
const plural = (count, word) => `${count} ${word}${count === 1 ? "" : "s"}`;
let state;
let filteredRuns = [];
let matrixOrder = modelIds.join(",");
let shareTimer;
let inspectedRun;
let columnCount = modelIds.length;

function readSelection(params, key, allowed, fallback) {
  if (!params.has(key)) return [...fallback];
  if (params.get(key) === "") return [];
  const selected = [...new Set(params.get(key).split(","))].filter(value => allowed.includes(value));
  return selected.length ? selected : [...fallback];
}

function readLocation() {
  const params = new URLSearchParams(window.location.search);
  const legacyProvider = params.get("provider");
  state = {
    providers: readSelection(params, "providers", providerIds, providerIds.includes(legacyProvider) ? [legacyProvider] : providerIds),
    models: readSelection(params, "models", modelIds, modelIds),
    task: data.tasks.includes(params.get("task")) ? params.get("task") : "all",
    query: params.get("q") || "",
    view: ["runs", "data"].includes(params.get("view")) ? "runs" : "artwork",
    zoom: ["2", "3", "4", "6"].includes(params.get("zoom")) ? Number(params.get("zoom")) : window.innerWidth <= 600 ? 2 : 3,
    background: ["grid", "light", "dark"].includes(params.get("background")) ? params.get("background") : "grid",
    stats: params.get("stats") !== "0",
    sort: sortKeys.includes(params.get("sort")) ? params.get("sort") : "task",
    direction: params.get("direction") === "desc" ? "desc" : "asc",
  };
}

function writeLocation(mode = "push") {
  const url = new URL(window.location.href);
  const defaults = {
    providers: state.providers.join(",") === providerIds.join(",") ? null : state.providers.join(","),
    models: state.models.join(",") === modelIds.join(",") ? null : state.models.join(","),
    task: state.task === "all" ? null : state.task,
    q: state.query || null,
    view: state.view === "artwork" ? null : state.view,
    zoom: String(state.zoom),
    background: state.background === "grid" ? null : state.background,
    stats: state.stats ? null : "0",
    sort: state.sort === "task" ? null : state.sort,
    direction: state.direction === "asc" ? null : "desc",
  };
  url.searchParams.delete("provider");
  for (const [key, value] of Object.entries(defaults)) {
    if (value === null) url.searchParams.delete(key);
    else url.searchParams.set(key, value);
  }
  if (url.href !== window.location.href) window.history[mode === "replace" ? "replaceState" : "pushState"](null, "", url);
}

function sortValue(run) {
  const model = models.get(run.model);
  return ({task: run.task, model: `${model.name} ${model.effort}`, provider: model.vendor,
    calls: run.tool_calls, looks: run.looks, tokens: run.tokens})[state.sort];
}

function compareRuns(left, right) {
  const a = sortValue(left), b = sortValue(right);
  // Missing totals stay at the end in both directions, rather than appearing as zero.
  if (a == null || b == null) return a == null && b == null ? 0 : a == null ? 1 : -1;
  const order = typeof a === "number" ? a - b : a.localeCompare(b);
  return state.direction === "desc" ? -order : order;
}

function renderChips(activeModels) {
  const container = document.querySelector("#active-filters");
  container.replaceChildren();
  const chips = [];
  if (state.providers.length !== providerIds.length) {
    const labels = state.providers.map(provider => data.models.find(model => model.provider === provider).vendor);
    chips.push([labels.join(", ") || "No providers", () => { state.providers = [...providerIds]; }]);
  }
  if (state.models.length !== modelIds.length) chips.push([plural(activeModels.length, "model"), () => { state.models = [...modelIds]; }]);
  if (state.task !== "all") chips.push([`Brief: ${state.task[0].toUpperCase()}${state.task.slice(1)}`, () => { state.task = "all"; }]);
  if (state.query) chips.push([`Search: ${state.query}`, () => { state.query = ""; }]);
  for (const [label, clear] of chips) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "filter-chip";
    button.setAttribute("aria-label", `Clear filter: ${label}`);
    const close = document.createElement("span");
    close.textContent = "×";
    close.setAttribute("aria-hidden", "true");
    button.append(label, close);
    button.addEventListener("click", () => { clear(); render("push"); });
    container.append(button);
  }
  container.hidden = !chips.length;
  const badge = document.querySelector("#mobile-filter-count");
  badge.textContent = chips.length;
  badge.hidden = !chips.length;
}

function updateScrollHint() {
  const overflowing = scrollPanel.scrollWidth > scrollPanel.clientWidth + 1;
  document.querySelector("#table-navigation").hidden = !overflowing;
  document.querySelector("#previous-columns").disabled = scrollPanel.scrollLeft <= 1;
  document.querySelector("#next-columns").disabled = scrollPanel.scrollLeft + scrollPanel.clientWidth >= scrollPanel.scrollWidth - 1;
}

function columnMinimum() {
  return window.innerWidth <= 600 ? Math.max(122, 48 * state.zoom + 26) : Math.max(state.zoom === 2 ? 128 : 176, 48 * state.zoom + 32);
}

function sizeColumns() {
  const briefWidth = window.innerWidth <= 600 ? 110 : 156;
  const minimum = columnMinimum();
  const width = columnCount ? Math.max(minimum, (scrollPanel.clientWidth - briefWidth) / columnCount) : minimum;
  document.documentElement.style.setProperty("--model-width", `${width}px`);
  updateScrollHint();
}

function render(historyMode = null) {
  const activeModels = state.models.filter(id => state.providers.includes(models.get(id).provider));
  const query = state.query.trim().toLowerCase();
  const activeTasks = data.tasks.filter(task => (state.task === "all" || state.task === task) && task.includes(query));
  const modelSet = new Set(activeModels), taskSet = new Set(activeTasks);
  columnCount = activeModels.length;
  filteredRuns = sourceRuns.filter(run => modelSet.has(run.model) && taskSet.has(run.task)).sort(compareRuns);
  providerInputs.forEach(input => { input.checked = state.providers.includes(input.value); });
  modelInputs.forEach(input => {
    input.checked = state.models.includes(input.value);
    input.closest("label").hidden = !state.providers.includes(models.get(input.value).provider);
  });
  document.querySelector(".no-model-options").hidden = Boolean(state.providers.length);
  document.querySelector("#model-filter-count").textContent = activeModels.length;
  taskSelect.value = state.task;
  search.value = state.query;
  zoomSelect.value = state.zoom;
  statsInput.checked = state.stats;
  document.body.classList.toggle("hide-stats", !state.stats);
  for (const background of ["grid", "light", "dark"]) document.body.classList.toggle(`background-${background}`, background === state.background);
  document.documentElement.style.setProperty("--zoom", state.zoom);
  document.documentElement.style.setProperty("--model-width", `${columnMinimum()}px`);
  document.documentElement.style.setProperty("--model-count", activeModels.length);
  for (const {row, cells} of matrixRows) {
    if (row.dataset.taskRow) row.hidden = !taskSet.has(row.dataset.taskRow);
    for (const [id, cell] of cells) cell.hidden = !modelSet.has(id);
  }
  const order = [...activeModels, ...modelIds.filter(id => !modelSet.has(id))];
  if (order.join(",") !== matrixOrder) {
    for (const {row, cells} of matrixRows) row.append(...order.map(id => cells.get(id)));
    matrixOrder = order.join(",");
  }
  const runBody = runTable.tBodies[0];
  for (const row of runRows.values()) row.hidden = true;
  for (const run of filteredRuns) {
    const row = runRows.get(runKey(run));
    row.hidden = false;
    runBody.append(row);
  }
  matrix.hidden = state.view !== "artwork";
  runTable.hidden = state.view !== "runs";
  document.querySelector(".display-controls").hidden = state.view !== "artwork";
  for (const button of document.querySelectorAll("[data-view]")) button.setAttribute("aria-pressed", String(button.dataset.view === state.view));
  for (const button of document.querySelectorAll("[data-background]")) button.setAttribute("aria-pressed", String(button.dataset.background === state.background));
  for (const heading of document.querySelectorAll("[data-sort-heading]")) {
    const active = heading.dataset.sortHeading === state.sort;
    heading.setAttribute("aria-sort", active ? state.direction === "asc" ? "ascending" : "descending" : "none");
    heading.querySelector("use").setAttribute("href", active ? `#icon-sort-${state.direction === "asc" ? "up" : "down"}` : "#icon-sort");
  }
  const total = document.createElement("strong");
  total.textContent = plural(filteredRuns.length, "result");
  document.querySelector("#result-count").replaceChildren(total, ` · ${plural(activeTasks.length, "brief")} · ${plural(activeModels.length, "model")}`);
  document.querySelector(".empty-state").hidden = Boolean(filteredRuns.length);
  scrollPanel.hidden = !filteredRuns.length;
  document.querySelector("#export-csv").disabled = !filteredRuns.length;
  document.querySelector("#close-filters").textContent = `Show ${plural(filteredRuns.length, "result")}`;
  renderChips(activeModels);
  copyFallback.hidden = true;
  clearTimeout(shareTimer);
  shareButton.querySelector("span").textContent = "Share view";
  shareStatus.textContent = "";
  if (historyMode) writeLocation(historyMode);
  sizeColumns();
}

function resetFilters() {
  state.providers = [...providerIds];
  state.models = [...modelIds];
  state.task = "all";
  state.query = "";
  render("push");
}

function fitRunArtwork() {
  if (!inspectedRun) return;
  const canvas = inspectedRun.task === "beam" ? 48 : 32;
  const available = window.innerWidth <= 600 ? window.innerWidth - 60 : 340;
  const scale = Math.max(1, Math.min(6, Math.floor(available / canvas)));
  runDialog.style.setProperty("--inspect-size", `${canvas * scale}px`);
}

function openRun(key) {
  const run = runs.get(key), model = models.get(run.model);
  inspectedRun = run;
  document.querySelector("#run-provider").textContent = `${model.vendor}${model.effort ? ` · ${model.effort} effort` : ""}`;
  document.querySelector("#run-title").textContent = `${run.task[0].toUpperCase()}${run.task.slice(1)} · ${model.name}`;
  const image = document.querySelector("#run-image");
  image.src = `showcase/${run.gif}`;
  image.alt = `${run.task} animation by ${model.name}`;
  document.querySelector("#run-canvas").textContent = `${run.task === "beam" ? "48 × 48" : "32 × 32"} px · ${run.frames} frames · 10 fps`;
  document.querySelector("#run-calls").textContent = number(run.tool_calls);
  document.querySelector("#run-looks").textContent = number(run.looks);
  document.querySelector("#run-tokens").textContent = number(run.tokens);
  document.querySelector("#run-prompt").textContent = data.briefs[run.task];
  document.querySelector("#run-model").textContent = `Recorded model: ${run.model}`;
  document.querySelector("#run-gif").href = `showcase/${run.gif}`;
  document.querySelector("#run-replay").href = `showcase/${run.replay}`;
  fitRunArtwork();
  runDialog.showModal();
}

for (const input of providerInputs) input.addEventListener("change", () => {
  state.providers = providerInputs.filter(option => option.checked).map(option => option.value);
  render("push");
});
for (const input of modelInputs) input.addEventListener("change", () => {
  state.models = input.checked ? [...state.models, input.value] : state.models.filter(id => id !== input.value);
  render("push");
});
for (const button of document.querySelectorAll("[data-reset]")) button.addEventListener("click", resetFilters);
document.querySelector("#all-models").addEventListener("click", () => { state.models = [...modelIds]; render("push"); });
document.querySelector("#clear-models").addEventListener("click", () => { state.models = []; render("push"); });
document.querySelector("#latest-models").addEventListener("click", () => {
  state.models = state.providers.map(provider => data.models.find(model => model.provider === provider).id);
  render("push");
});
taskSelect.addEventListener("change", () => { state.task = taskSelect.value; render("push"); });
search.addEventListener("input", () => { state.query = search.value; render("replace"); });
zoomSelect.addEventListener("change", () => { state.zoom = Number(zoomSelect.value); render("push"); });
statsInput.addEventListener("change", () => { state.stats = statsInput.checked; render("push"); });
for (const button of document.querySelectorAll("[data-view]")) button.addEventListener("click", () => { state.view = button.dataset.view; render("push"); });
for (const button of document.querySelectorAll("[data-background]")) button.addEventListener("click", () => { state.background = button.dataset.background; render("push"); });
for (const button of document.querySelectorAll("[data-sort]")) button.addEventListener("click", () => {
  const sort = button.dataset.sort;
  state.direction = state.sort === sort ? state.direction === "asc" ? "desc" : "asc" : ["calls", "looks", "tokens"].includes(sort) ? "desc" : "asc";
  state.sort = sort;
  render("push");
});
shareButton.addEventListener("click", async () => {
  writeLocation("replace");
  const url = new URL(window.location.href);
  url.hash = "gallery";
  window.history.replaceState(null, "", url);
  try {
    await navigator.clipboard.writeText(url.href);
    shareButton.querySelector("span").textContent = "Link copied";
    shareStatus.textContent = "View link copied to clipboard";
    shareTimer = setTimeout(() => { shareButton.querySelector("span").textContent = "Share view"; shareStatus.textContent = ""; }, 2500);
  } catch {
    copyFallback.hidden = false;
    const input = document.querySelector("#share-url");
    input.value = url.href;
    input.focus();
    input.select();
  }
});
document.querySelector("#export-csv").addEventListener("click", () => {
  const rows = [["Brief", "Model", "Provider", "Calls", "Looks", "Reported tokens", "Frames", "GIF", "Replay"],
    ...filteredRuns.map(run => [run.task, run.model, run.vendor, run.tool_calls, run.looks, run.tokens, run.frames,
      new URL(`showcase/${run.gif}`, window.location.href).href, new URL(`showcase/${run.replay}`, window.location.href).href])];
  const csv = rows.map(row => row.map(value => `"${String(value ?? "").replaceAll('"', '""')}"`).join(",")).join("\r\n") + "\r\n";
  const url = URL.createObjectURL(new Blob([csv], {type: "text/csv;charset=utf-8"}));
  const link = document.createElement("a");
  link.href = url;
  link.download = "atelier-runs.csv";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
for (const [id, direction] of [["previous-columns", -1], ["next-columns", 1]]) document.querySelector(`#${id}`).addEventListener("click", () => {
  scrollPanel.scrollBy({left: columnMinimum() * 2 * direction});
});
scrollPanel.addEventListener("scroll", updateScrollHint);
for (const link of document.querySelectorAll("[data-run], [data-brief]")) link.addEventListener("click", event => {
  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  if (link.dataset.run) openRun(link.dataset.run);
  else {
    const task = link.dataset.brief;
    document.querySelector("#brief-title").textContent = `${task[0].toUpperCase()}${task.slice(1)}`;
    document.querySelector("#brief-prompt").textContent = data.briefs[task];
    briefDialog.showModal();
  }
});
document.querySelector("#open-filters").addEventListener("click", () => {
  filterDialog.querySelector(".filter-dialog-content").append(filterPanel);
  filterDialog.showModal();
});
document.querySelector("#close-filters").addEventListener("click", () => filterDialog.close());
filterDialog.addEventListener("close", () => filterHost.append(filterPanel));
for (const dialog of [filterDialog, runDialog, briefDialog]) dialog.addEventListener("click", event => {
  const rect = dialog.getBoundingClientRect();
  if (event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom)) dialog.close();
});
for (const link of document.querySelectorAll('a[href="#about"]')) link.addEventListener("click", () => { document.querySelector("#about").open = true; });
window.addEventListener("popstate", () => { readLocation(); render(); });
window.addEventListener("resize", () => {
  if (window.innerWidth > 900 && filterDialog.open) filterDialog.close();
  if (runDialog.open) fitRunArtwork();
  sizeColumns();
});
readLocation();
document.documentElement.classList.add("js");
render();
if (window.location.hash === "#about") document.querySelector("#about").open = true;
