export type Provider = 'claude' | 'codex' | 'kimi';
export type View = 'artwork' | 'models' | 'runs';
export type Sort = 'task' | 'model' | 'provider' | 'calls' | 'looks' | 'tokens';
export type Background = 'grid' | 'light' | 'dark';

export interface Run {
  task: string;
  model: string;
  vendor: string;
  gif: string;
  replay: string;
  tool_calls: number;
  looks: number;
  tokens: number | null;
  frames: number;
}

export interface Dataset {
  models: string[];
  tasks: string[];
  runs: Run[];
  method: string;
  server: string;
  verified: string;
}

export interface Model {
  id: string;
  name: string;
  /** Lowercase model line shared across versions, such as `haiku` or `gpt`. */
  family: string;
  /** Recorded reasoning effort, or an empty string when none was recorded. */
  effort: string;
  provider: Provider;
  vendor: string;
}

export interface ComparisonState {
  /** Model lines to show; an empty list shows every line. */
  families: string[];
  /** Effort levels to show (`none` for unrecorded); an empty list shows every level. */
  efforts: string[];
  /** Individually shown models, in column order. */
  models: string[];
  task: string;
  query: string;
  view: View;
  zoom: number;
  background: Background;
  stats: boolean;
  sort: Sort;
  direction: 'asc' | 'desc';
}

export const providers: { id: Provider; name: string }[] = [
  { id: 'claude', name: 'Anthropic' },
  { id: 'codex', name: 'OpenAI' },
  { id: 'kimi', name: 'Moonshot AI' },
];
export const views: View[] = ['artwork', 'models', 'runs'];
export const sortKeys: Sort[] = ['task', 'model', 'provider', 'calls', 'looks', 'tokens'];
export const title = (value: string) => value.charAt(0).toUpperCase() + value.slice(1);
export const number = (value: number | null) =>
  value === null ? '—' : value.toLocaleString('en-US');
export const runKey = (run: Run) => `${run.model}/${run.task}`;
export const canvasSize = (task: string) => (task === 'beam' ? 48 : 32);
export const familyName = (family: string) => (family === 'gpt' ? 'GPT' : title(family));
export const effortKey = (model: Model) => model.effort || 'none';
export const effortName = (effort: string) => (effort === 'none' ? 'unrecorded' : effort);

export function getModels(data: Dataset): Model[] {
  const groups = providers.map((provider) =>
    [...data.models]
      .reverse()
      .filter((id) => data.runs.find((run) => run.model === id)?.vendor === provider.name)
      .map((id) => {
        const match = id.match(/-(low|medium|high|xhigh|max)$/);
        const effort = match?.[1] ?? '';
        const base = effort ? id.slice(0, -(effort.length + 1)) : id;
        const name = base.startsWith('gpt-')
          ? `GPT-${base.slice(4).split('-').map(title).join(' ')}`
          : base.split('-').map(title).join(' ');
        return {
          id,
          name,
          family: base.split('-')[0],
          effort,
          provider: provider.id,
          vendor: provider.name,
        };
      }),
  );
  // Interleave providers, with their last recorded model first. This is not a ranking.
  return Array.from({ length: Math.max(...groups.map((group) => group.length)) }, (_, index) =>
    groups.flatMap((group) => (group[index] ? [group[index]] : [])),
  ).flat();
}

/** Distinct values in first-seen order. */
const distinct = (values: string[]) => [...new Set(values)];
export const familiesOf = (models: Model[]) =>
  providers.flatMap((provider) =>
    distinct(models.filter((model) => model.provider === provider.id).map((m) => m.family)),
  );
export const effortsOf = (models: Model[]) =>
  ['xhigh', 'max', 'high', 'medium', 'low', 'none'].filter((effort) =>
    models.some((model) => effortKey(model) === effort),
  );

export function defaultState(models: Model[]): ComparisonState {
  return {
    families: [],
    efforts: [],
    models: models.map((model) => model.id),
    task: 'all',
    query: '',
    view: 'artwork',
    zoom: 2,
    background: 'grid',
    stats: false,
    sort: 'task',
    direction: 'asc',
  };
}

function selection(params: URLSearchParams, key: string, allowed: string[], fallback: string[]) {
  if (!params.has(key)) return [...fallback];
  if (params.get(key) === '') return [];
  const values = distinct(params.get(key)!.split(',')).filter((value) => allowed.includes(value));
  return values.length ? values : [...fallback];
}

export function readState(search: string, models: Model[], tasks: string[]): ComparisonState {
  const params = new URLSearchParams(search);
  const defaults = defaultState(models);
  return {
    families: selection(params, 'families', familiesOf(models), []),
    efforts: selection(params, 'efforts', effortsOf(models), []),
    models: selection(params, 'models', defaults.models, defaults.models),
    task: tasks.includes(params.get('task') ?? '') ? params.get('task')! : 'all',
    query: params.get('q') ?? '',
    view: views.includes(params.get('view') as View) ? (params.get('view') as View) : 'artwork',
    zoom: ['2', '3', '4', '6'].includes(params.get('zoom') ?? '') ? Number(params.get('zoom')) : 2,
    background: ['grid', 'light', 'dark'].includes(params.get('background') ?? '')
      ? (params.get('background') as Background)
      : 'grid',
    stats: params.get('stats') === '1',
    sort: sortKeys.includes(params.get('sort') as Sort) ? (params.get('sort') as Sort) : 'task',
    direction: params.get('direction') === 'desc' ? 'desc' : 'asc',
  };
}

export function stateURL(state: ComparisonState, models: Model[], current: string): URL {
  const url = new URL(current);
  const defaults = defaultState(models);
  const values: Record<string, string | null> = {
    families: state.families.join(',') || null,
    efforts: state.efforts.join(',') || null,
    models: state.models.join(',') === defaults.models.join(',') ? null : state.models.join(','),
    task: state.task === 'all' ? null : state.task,
    q: state.query || null,
    view: state.view === 'artwork' ? null : state.view,
    zoom: state.zoom === 2 ? null : String(state.zoom),
    background: state.background === 'grid' ? null : state.background,
    stats: state.stats ? '1' : null,
    sort: state.sort === 'task' ? null : state.sort,
    direction: state.direction === 'asc' ? null : 'desc',
  };
  for (const [key, value] of Object.entries(values)) {
    if (value === null) url.searchParams.delete(key);
    else url.searchParams.set(key, value);
  }
  return url;
}

const words = (text: string) =>
  text
    .toLowerCase()
    .split(/[\s-]+/)
    .filter(Boolean);
const modelWords = (model: Model) =>
  words(
    `${model.name} ${model.id} ${model.vendor} ${model.provider} ${effortName(effortKey(model))}`,
  );

export interface Command {
  /** Models matching the command, or null when it names no model. */
  models: Set<string> | null;
  /** Briefs matching the command, or null when it names no brief. */
  tasks: Set<string> | null;
  /** Words that match neither a model nor a brief. */
  unknown: string[];
}

/**
 * Read a typed command such as `haiku`, `opus xhigh`, or `sonnet, gpt cat`.
 *
 * Each word matches the start of a word in a model's name, identifier, provider,
 * or effort, or the start of a brief's name. Words in one comma-separated group
 * narrow the models together; groups add their models to each other. Brief words
 * from every group are combined.
 */
export function parseCommand(query: string, models: Model[], tasks: string[]): Command {
  const command: Command = { models: null, tasks: null, unknown: [] };
  for (const group of query.split(',')) {
    let matched: Model[] | null = null;
    for (const word of words(group)) {
      const named = models.filter((model) => modelWords(model).some((w) => w.startsWith(word)));
      const briefs = tasks.filter((task) => task.startsWith(word));
      if (named.length) matched = (matched ?? models).filter((model) => named.includes(model));
      if (briefs.length) command.tasks = new Set([...(command.tasks ?? []), ...briefs]);
      if (!named.length && !briefs.length) command.unknown.push(word);
    }
    if (matched)
      command.models = new Set([...(command.models ?? []), ...matched.map((model) => model.id)]);
  }
  if (command.unknown.length) {
    command.models = new Set();
    command.tasks = new Set();
  }
  return command;
}

export function visibleModels(state: ComparisonState, models: Model[], tasks: string[]): Model[] {
  const command = parseCommand(state.query, models, tasks);
  return state.models.flatMap((id) => {
    const model = models.find((model) => model.id === id);
    return model &&
      (!state.families.length || state.families.includes(model.family)) &&
      (!state.efforts.length || state.efforts.includes(effortKey(model))) &&
      (!command.models || command.models.has(id))
      ? [model]
      : [];
  });
}

export function visibleTasks(state: ComparisonState, models: Model[], tasks: string[]): string[] {
  const command = parseCommand(state.query, models, tasks);
  return tasks.filter(
    (task) =>
      (state.task === 'all' || task === state.task) && (!command.tasks || command.tasks.has(task)),
  );
}

export function sortRuns(
  runs: Run[],
  models: Model[],
  sort: Sort,
  direction: ComparisonState['direction'],
): Run[] {
  const value = (run: Run): string | number | null => {
    const model = models.find((model) => model.id === run.model)!;
    return {
      task: run.task,
      model: `${model.name} ${model.effort}`,
      provider: run.vendor,
      calls: run.tool_calls,
      looks: run.looks,
      tokens: run.tokens,
    }[sort];
  };
  return [...runs].sort((left, right) => {
    const a = value(left),
      b = value(right);
    // Missing totals stay last in either direction and never become zero.
    if (a === null || b === null) return a === b ? 0 : a === null ? 1 : -1;
    const difference =
      typeof a === 'number' && typeof b === 'number'
        ? a - b
        : String(a).localeCompare(String(b), 'en');
    return direction === 'desc' ? -difference : difference;
  });
}

export function filterRuns(data: Dataset, state: ComparisonState, models: Model[]): Run[] {
  const ids = new Set(visibleModels(state, models, data.tasks).map((model) => model.id));
  const tasks = new Set(visibleTasks(state, models, data.tasks));
  return sortRuns(
    data.runs.filter((run) => ids.has(run.model) && tasks.has(run.task)),
    models,
    state.sort,
    state.direction,
  );
}

export function runsCSV(runs: Run[], current: string): string {
  const rows = [
    ['Brief', 'Model', 'Provider', 'Calls', 'Looks', 'Reported tokens', 'Frames', 'GIF', 'Replay'],
    ...runs.map((run) => [
      run.task,
      run.model,
      run.vendor,
      run.tool_calls,
      run.looks,
      run.tokens,
      run.frames,
      new URL(`showcase/${run.gif}`, current).href,
      new URL(`showcase/${run.replay}`, current).href,
    ]),
  ];
  return (
    rows
      .map((row) => row.map((value) => `"${String(value ?? '').replaceAll('"', '""')}"`).join(','))
      .join('\r\n') + '\r\n'
  );
}
