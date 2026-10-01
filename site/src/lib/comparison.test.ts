import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  defaultState,
  filterRuns,
  getModels,
  readState,
  runsCSV,
  sortRuns,
  stateURL,
  visibleModels,
  visibleTasks,
} from './comparison.ts';
import type { Dataset } from './comparison.ts';

const data: Dataset = JSON.parse(
  readFileSync(new URL('../../../showcase/runs.json', import.meta.url), 'utf8'),
);
const models = getModels(data);

test('the default comparison includes every original model and run', () => {
  const state = defaultState(models);
  assert.equal(
    new Set(visibleModels(state, models).map((model) => model.id)).size,
    data.models.length,
  );
  assert.equal(filterRuns(data, state, models).length, data.runs.length);
  assert.equal(models[0].vendor, 'Anthropic');
  assert.equal(models[1].vendor, 'OpenAI');
  assert.equal(models[2].vendor, 'Moonshot AI');
});

test('provider, model, brief, and search filters combine', () => {
  const state = {
    ...defaultState(models),
    providers: ['codex'] as const,
    models: [models[0].id, models[1].id],
    task: 'cat',
    query: ' CAT ',
  };
  const results = filterRuns(data, { ...state, providers: [...state.providers] }, models);
  assert.equal(results.length, 1);
  assert.equal(results[0].model, models[1].id);
  assert.equal(results[0].task, 'cat');
  assert.deepEqual(
    visibleTasks({ ...defaultState(models), task: 'cat', query: 'alien' }, data.tasks),
    [],
  );
});

test('empty selections survive shared-link round trips', () => {
  for (const patch of [{ models: [] }, { providers: [] }]) {
    const state = { ...defaultState(models), ...patch };
    const url = stateURL(state, models, 'https://example.test/atelier/');
    const restored = readState(url.search, models, data.tasks);
    assert.deepEqual(restored, state);
    assert.equal(filterRuns(data, restored, models).length, 0);
  }
});

test('shared links preserve column order and every comparison preference', () => {
  const state = {
    ...defaultState(models),
    models: [models[2].id, models[0].id],
    task: 'beam',
    query: 'be',
    view: 'runs' as const,
    zoom: 6,
    background: 'dark' as const,
    stats: true,
    sort: 'tokens' as const,
    direction: 'desc' as const,
  };
  const url = stateURL(state, models, 'https://example.test/atelier/?provider=claude');
  assert.equal(url.pathname, '/atelier/');
  assert.equal(url.searchParams.has('provider'), false);
  assert.deepEqual(readState(url.search, models, data.tasks), state);
});

test('invalid identifiers fall back safely and valid mixed selections are deduplicated', () => {
  assert.deepEqual(
    readState('?models=unknown&providers=unknown&zoom=999&task=unknown', models, data.tasks),
    defaultState(models),
  );
  const state = readState(`?models=unknown,${models[0].id},${models[0].id}`, models, data.tasks);
  assert.deepEqual(state.models, [models[0].id]);
});

test('legacy provider and comparison links still select their original models', () => {
  const state = readState(
    `?provider=kimi&view=compare&models=${models[2].id}&task=ball`,
    models,
    data.tasks,
  );
  const runs = filterRuns(data, state, models);
  assert.equal(runs.length, 1);
  assert.equal(runs[0].vendor, 'Moonshot AI');
  assert.equal(runs[0].task, 'ball');
});

test('token sorting is numeric with unreported values last in both directions', () => {
  for (const direction of ['asc', 'desc'] as const) {
    const runs = sortRuns(data.runs, models, 'tokens', direction);
    const firstMissing = runs.findIndex((run) => run.tokens === null);
    assert.ok(firstMissing > 0);
    assert.ok(runs.slice(firstMissing).every((run) => run.tokens === null));
    for (let i = 1; i < firstMissing; i++) {
      assert.ok(
        direction === 'asc'
          ? runs[i - 1].tokens! <= runs[i].tokens!
          : runs[i - 1].tokens! >= runs[i].tokens!,
      );
    }
  }
});

test('CSV uses original counts, blank missing totals, and artifact URLs under the Pages prefix', () => {
  const run = data.runs.find((run) => run.tokens === null)!;
  const csv = runsCSV([run], 'https://example.test/atelier/?task=cat#gallery');
  assert.ok(csv.includes(`"${run.tool_calls}","${run.looks}","","${run.frames}"`));
  assert.ok(csv.includes(`"https://example.test/atelier/showcase/${run.gif}"`));
  assert.ok(csv.includes(`"https://example.test/atelier/showcase/${run.replay}"`));
  assert.equal(csv.split('\r\n').length, 3);
  assert.ok(
    runsCSV([{ ...run, model: 'a "quoted", model' }], 'https://example.test/').includes(
      '"a ""quoted"", model"',
    ),
  );
});
