import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  defaultState,
  effortsOf,
  familiesOf,
  filterRuns,
  getModels,
  parseCommand,
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
const shown = (patch: object) =>
  visibleModels({ ...defaultState(models), ...patch }, models, data.tasks).map((model) => model.id);

test('the default comparison includes every original model and run', () => {
  const state = defaultState(models);
  assert.equal(new Set(shown({})).size, data.models.length);
  assert.equal(filterRuns(data, state, models).length, data.runs.length);
  assert.equal(models[0].vendor, 'Anthropic');
  assert.equal(models[1].vendor, 'OpenAI');
  assert.equal(models[2].vendor, 'Moonshot AI');
});

test('every model belongs to one model line and one effort level', () => {
  const families = familiesOf(models);
  assert.ok(families.includes('haiku') && families.includes('gpt') && families.includes('kimi'));
  assert.equal(
    families.reduce((count, family) => count + shown({ families: [family] }).length, 0),
    models.length,
  );
  assert.equal(
    effortsOf(models).reduce((count, effort) => count + shown({ efforts: [effort] }).length, 0),
    models.length,
  );
  assert.ok(shown({ families: ['haiku'] }).every((id) => id.startsWith('haiku-')));
  assert.ok(shown({ efforts: ['xhigh'] }).every((id) => id.endsWith('-xhigh')));
});

test('model line, effort, hidden model, brief, and command filters combine', () => {
  const opus = shown({ families: ['opus'] });
  assert.ok(opus.length > 1);
  assert.deepEqual(shown({ families: ['opus'], efforts: ['xhigh'] }), ['opus-5.5-xhigh']);
  assert.deepEqual(shown({ families: ['opus'], models: [opus[1]] }), [opus[1]]);
  const results = filterRuns(
    data,
    {
      ...defaultState(models),
      families: ['opus'],
      efforts: ['xhigh'],
      task: 'cat',
      query: ' CAT ',
    },
    models,
  );
  assert.deepEqual(
    results.map((run) => `${run.model}/${run.task}`),
    ['opus-5.5-xhigh/cat'],
  );
  assert.deepEqual(
    visibleTasks({ ...defaultState(models), task: 'cat', query: 'alien' }, models, data.tasks),
    [],
  );
});

test('typed commands name models and briefs by the start of a word', () => {
  assert.deepEqual(shown({ query: 'haiku' }), shown({ families: ['haiku'] }));
  assert.deepEqual(shown({ query: 'Opus XHIGH' }), ['opus-5.5-xhigh']);
  assert.deepEqual(
    shown({ query: 'anthropic' }),
    shown({}).filter((id) => !/^(gpt|kimi)/.test(id)),
  );
  assert.deepEqual(
    new Set(shown({ query: 'sonnet, gpt' })),
    new Set([...shown({ families: ['sonnet'] }), ...shown({ families: ['gpt'] })]),
  );
  // "son" starts "sonnet" but only sits inside "person".
  const command = parseCommand('son', models, data.tasks);
  assert.equal(command.tasks, null);
  assert.ok(command.models!.size > 0);
  const mixed = filterRuns(data, { ...defaultState(models), query: 'haiku cat torch' }, models);
  assert.deepEqual(new Set(mixed.map((run) => run.task)), new Set(['cat', 'torch']));
  assert.ok(mixed.every((run) => run.model.startsWith('haiku-')));
  assert.deepEqual(parseCommand('haiku zzz', models, data.tasks).unknown, ['zzz']);
  assert.equal(filterRuns(data, { ...defaultState(models), query: 'haiku zzz' }, models).length, 0);
});

test('an empty model selection survives shared-link round trips', () => {
  const state = { ...defaultState(models), models: [] };
  const url = stateURL(state, models, 'https://example.test/atelier/');
  const restored = readState(url.search, models, data.tasks);
  assert.deepEqual(restored, state);
  assert.equal(filterRuns(data, restored, models).length, 0);
});

test('shared links preserve column order and every comparison preference', () => {
  const state = {
    ...defaultState(models),
    families: ['haiku', 'opus'],
    efforts: ['xhigh'],
    models: [models[2].id, models[0].id],
    task: 'beam',
    query: 'be',
    view: 'models' as const,
    zoom: 6,
    background: 'dark' as const,
    stats: true,
    sort: 'tokens' as const,
    direction: 'desc' as const,
  };
  const url = stateURL(state, models, 'https://example.test/atelier/?view=artwork');
  assert.equal(url.pathname, '/atelier/');
  assert.deepEqual(readState(url.search, models, data.tasks), state);
  assert.equal(stateURL(defaultState(models), models, 'https://example.test/').search, '');
});

test('invalid identifiers fall back safely and valid mixed selections are deduplicated', () => {
  assert.deepEqual(
    readState(
      '?models=unknown&families=unknown&efforts=unknown&zoom=999&task=unknown&view=unknown',
      models,
      data.tasks,
    ),
    defaultState(models),
  );
  const state = readState(`?models=unknown,${models[0].id},${models[0].id}`, models, data.tasks);
  assert.deepEqual(state.models, [models[0].id]);
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
