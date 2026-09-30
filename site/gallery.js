"use strict";

const taskSelect = document.querySelector("#task-select");
const filters = [...document.querySelectorAll("[data-filter]")];
const tasks = [...document.querySelectorAll("[data-task]")];
const resultCount = document.querySelector(".result-count");
let provider = "all";

function readLocation() {
  const params = new URLSearchParams(window.location.search);
  const task = params.get("task") || tasks[0].dataset.task;
  taskSelect.value = [...taskSelect.options].some(option => option.value === task)
    ? task : tasks[0].dataset.task;
  const requested = params.get("provider") || "all";
  provider = filters.some(button => button.dataset.filter === requested) ? requested : "all";
}

function render(updateLocation = false) {
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
  for (const button of filters) {
    button.setAttribute("aria-pressed", String(button.dataset.filter === provider));
  }
  const taskName = taskSelect.value === "all" ? "all briefs" : taskSelect.selectedOptions[0].text;
  resultCount.textContent = `${count} ${count === 1 ? "animation" : "animations"} · ${taskName} · ${visibleProviders.size} ${visibleProviders.size === 1 ? "provider" : "providers"}`;
  if (updateLocation) {
    const url = new URL(window.location.href);
    url.searchParams.set("task", taskSelect.value);
    if (provider === "all") url.searchParams.delete("provider");
    else url.searchParams.set("provider", provider);
    window.history.replaceState(null, "", url);
  }
}

taskSelect.addEventListener("change", () => render(true));
for (const button of filters) {
  button.addEventListener("click", () => {
    provider = button.dataset.filter;
    render(true);
  });
}
window.addEventListener("popstate", () => { readLocation(); render(); });
readLocation();
render();
