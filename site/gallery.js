"use strict";

const taskSelect = document.querySelector("#task-select");
const filters = [...document.querySelectorAll("[data-filter]")];
const tasks = [...document.querySelectorAll("[data-task]")];
const resultCount = document.querySelector(".result-count");
const viewButtons = [...document.querySelectorAll("[data-view]")];
const modelSelects = [...document.querySelectorAll("[data-model-select]")];
const zoomButtons = [...document.querySelectorAll("[data-zoom]")];
const backgroundButtons = [...document.querySelectorAll("[data-background]")];
const comparison = document.querySelector("#comparison");
const columns = document.querySelector(".comparison-columns");
const shareButton = document.querySelector("#share-link");
const shareFeedback = document.querySelector(".share-feedback");
const modelIds = new Set([...modelSelects[0].options].map(option => option.value).filter(Boolean));
const defaultModels = modelSelects.map(select => select.dataset.default);
const taskIds = tasks.map(task => task.dataset.task);
const cards = new Map(tasks.map(task => [task.dataset.task,
  new Map([...task.querySelectorAll(".card")].map(card => [card.dataset.model, card]))]));
let view = "compare";
let provider = "all";
let selectedModels = defaultModels;
let zoom = 4;
let background = "grid";
let comparisonKey = "";
let shareTimer;

function readLocation() {
  const params = new URLSearchParams(window.location.search);
  const requestedTask = params.get("task") || taskIds[0];
  taskSelect.value = requestedTask === "all" || taskIds.includes(requestedTask) ? requestedTask : taskIds[0];
  const requestedView = params.get("view");
  view = ["compare", "gallery"].includes(requestedView) ? requestedView
    : params.has("provider") || taskSelect.value === "all" ? "gallery" : "compare";
  if (view === "compare" && taskSelect.value === "all") taskSelect.value = taskIds[0];
  const requestedProvider = params.get("provider") || "all";
  provider = filters.some(button => button.dataset.filter === requestedProvider) ? requestedProvider : "all";
  const requestedModels = [...new Set((params.get("models") || "").split(","))].filter(model => modelIds.has(model));
  selectedModels = requestedModels.length >= 2 ? requestedModels.slice(0, modelSelects.length) : [...defaultModels];
  zoom = zoomButtons.some(button => button.dataset.zoom === params.get("zoom")) ? Number(params.get("zoom")) : 4;
  background = backgroundButtons.some(button => button.dataset.background === params.get("background"))
    ? params.get("background") : "grid";
}

function writeLocation(push = true) {
  const url = new URL(window.location.href);
  url.searchParams.set("task", taskSelect.value);
  url.searchParams.set("models", selectedModels.join(","));
  if (view === "compare") url.searchParams.delete("view");
  else url.searchParams.set("view", view);
  if (view === "gallery" && provider !== "all") url.searchParams.set("provider", provider);
  else url.searchParams.delete("provider");
  if (zoom === 4) url.searchParams.delete("zoom");
  else url.searchParams.set("zoom", String(zoom));
  if (background === "grid") url.searchParams.delete("background");
  else url.searchParams.set("background", background);
  if (url.href !== window.location.href) window.history[push ? "pushState" : "replaceState"](null, "", url);
}

function syncModelSelects() {
  modelSelects.forEach((select, index) => {
    select.value = selectedModels[index] || "";
    for (const option of select.options) {
      option.disabled = Boolean(option.value) && option.value !== select.value && selectedModels.includes(option.value);
    }
    select.closest(".comparison-column").classList.toggle("is-empty", !select.value);
  });
}

function renderComparison() {
  const task = tasks.find(section => section.dataset.task === taskSelect.value);
  document.querySelector("#comparison-title").textContent = task.querySelector("h2").textContent;
  document.querySelector("#comparison-canvas").textContent = task.querySelector(".canvas-label").textContent;
  document.querySelector("#comparison-brief p").textContent = task.querySelector(".brief p").textContent;
  syncModelSelects();
  columns.style.setProperty("--columns", selectedModels.length);
  const canvasSize = Number(cards.get(taskSelect.value).get(selectedModels[0]).dataset.size) / 4;
  columns.style.setProperty("--column-width", `${Math.max(280, canvasSize * zoom + 32)}px`);
  comparison.style.setProperty("--display-size", `${canvasSize * zoom}px`);
  comparison.style.setProperty("--stage-height", `${Math.max(240, canvasSize * zoom + 56)}px`);
  comparison.dataset.background = background;
  for (const button of zoomButtons) button.setAttribute("aria-pressed", String(Number(button.dataset.zoom) === zoom));
  for (const button of backgroundButtons) button.setAttribute("aria-pressed", String(button.dataset.background === background));

  const key = `${taskSelect.value}/${selectedModels.join(",")}`;
  if (key === comparisonKey) return;
  comparisonKey = key;
  modelSelects.forEach((select, index) => {
    const slot = select.closest(".comparison-column").querySelector(".comparison-slot");
    slot.replaceChildren();
    const model = selectedModels[index];
    if (!model) return;
    const card = cards.get(taskSelect.value).get(model).cloneNode(true);
    const heading = card.querySelector(".card-heading");
    heading.querySelector("h4").classList.add("visually-hidden");
    const label = document.createElement("p");
    label.className = "comparison-provider";
    const dot = document.createElement("span");
    dot.className = `provider-dot ${card.dataset.providerKey}`;
    dot.setAttribute("aria-hidden", "true");
    label.append(dot, `${card.dataset.providerName} / ${card.dataset.vendor}`);
    heading.prepend(label);
    const tokens = document.createElement("p");
    tokens.className = "comparison-tokens";
    const total = document.createElement("strong");
    total.textContent = card.dataset.tokens;
    tokens.append("Reported tokens", total);
    card.querySelector(".run-details").before(tokens);
    // A comparison can scroll horizontally; load its three small GIFs immediately.
    card.querySelector("img").loading = "eager";
    slot.append(card);
  });
}

function render(updateLocation = false) {
  const comparing = view === "compare";
  document.querySelector("#browse-gallery").hidden = comparing;
  comparison.hidden = !comparing;
  document.querySelector("#provider-picker").hidden = comparing;
  document.querySelector(".brief-arrows").hidden = !comparing;
  [...taskSelect.options].find(option => option.value === "all").disabled = comparing;
  for (const button of viewButtons) button.setAttribute("aria-pressed", String(button.dataset.view === view));
  let count = 0;
  const visibleProviders = new Set();
  for (const task of tasks) {
    task.hidden = taskSelect.value !== "all" && taskSelect.value !== task.dataset.task;
    for (const group of task.querySelectorAll("[data-provider]")) {
      group.hidden = provider !== "all" && provider !== group.dataset.provider;
      if (!task.hidden && !group.hidden) {
        count += group.querySelectorAll(".card").length;
        visibleProviders.add(group.dataset.provider);
      }
    }
  }
  if (comparing) {
    renderComparison();
    count = selectedModels.length;
    visibleProviders.clear();
    for (const model of selectedModels) visibleProviders.add(cards.get(taskSelect.value).get(model).dataset.providerKey);
  }
  for (const button of filters) button.setAttribute("aria-pressed", String(button.dataset.filter === provider));
  const taskName = taskSelect.selectedOptions[0].text;
  resultCount.textContent = `${count} ${comparing ? "models" : count === 1 ? "animation" : "animations"} · ${taskName} · ${visibleProviders.size} ${visibleProviders.size === 1 ? "provider" : "providers"}`;
  shareFeedback.hidden = true;
  clearTimeout(shareTimer);
  shareButton.textContent = "Copy view link ↗";
  document.querySelector("#share-status").textContent = "";
  if (updateLocation) writeLocation();
}

taskSelect.addEventListener("change", () => render(true));
for (const button of filters) {
  button.addEventListener("click", () => {
    provider = button.dataset.filter;
    render(true);
  });
}
window.addEventListener("popstate", () => { readLocation(); render(); });
for (const button of viewButtons) {
  button.addEventListener("click", () => {
    view = button.dataset.view;
    if (view === "compare" && taskSelect.value === "all") taskSelect.value = taskIds[0];
    render(true);
  });
}
for (const select of modelSelects) {
  select.addEventListener("change", () => {
    const models = modelSelects.map(picker => picker.value).filter(Boolean);
    if (models.length < 2 || new Set(models).size !== models.length) { syncModelSelects(); return; }
    selectedModels = models;
    render(true);
  });
}
for (const button of zoomButtons) button.addEventListener("click", () => { zoom = Number(button.dataset.zoom); render(true); });
for (const button of backgroundButtons) button.addEventListener("click", () => { background = button.dataset.background; render(true); });
for (const [id, direction] of [["previous-brief", -1], ["next-brief", 1]]) {
  document.querySelector(`#${id}`).addEventListener("click", () => {
    const index = taskIds.indexOf(taskSelect.value);
    taskSelect.value = taskIds[(index + direction + taskIds.length) % taskIds.length];
    render(true);
  });
}
shareButton.addEventListener("click", async () => {
  writeLocation(false);
  const url = new URL(window.location.href);
  url.hash = "gallery";
  window.history.replaceState(null, "", url);
  try {
    await navigator.clipboard.writeText(window.location.href);
    shareButton.textContent = "Link copied ✓";
    document.querySelector("#share-status").textContent = "View link copied to clipboard";
    shareTimer = setTimeout(() => { shareButton.textContent = "Copy view link ↗"; }, 2500);
  } catch {
    shareFeedback.hidden = false;
    const input = document.querySelector("#share-url");
    input.value = window.location.href;
    input.focus();
    input.select();
  }
});
readLocation();
render();
document.querySelector(".gallery-intro").hidden = false;
shareButton.hidden = false;
