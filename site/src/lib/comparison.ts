export type Provider = 'claude' | 'codex' | 'kimi';
export type View = 'artwork' | 'runs';
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
  effort: string;
  provider: Provider;
  vendor: string;
}

export interface ComparisonState {
  providers: Provider[];
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

export const providers: { id: Provider; name: string; initial: string }[] = [
  { id: 'claude', name: 'Anthropic', initial: 'A' },
  { id: 'codex', name: 'OpenAI', initial: 'O' },
  { id: 'kimi', name: 'Moonshot AI', initial: 'K' },
];
export const sortKeys: Sort[] = ['task', 'model', 'provider', 'calls', 'looks', 'tokens'];
export const title = (value: string) => value.charAt(0).toUpperCase() + value.slice(1);
export const number = (value: number | null) =>
  value === null ? '—' : value.toLocaleString('en-US');
export const runKey = (run: Run) => `${run.model}/${run.task}`;
export const canvasSize = (task: string) => (task === 'beam' ? 48 : 32);

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
        return { id, name, effort, provider: provider.id, vendor: provider.name };
      }),
  );
  // Interleave providers, with their last recorded model first. This is not a ranking.
  return Array.from({ length: Math.max(...groups.map((group) => group.length)) }, (_, index) =>
    groups.flatMap((group) => (group[index] ? [group[index]] : [])),
  ).flat();
}

export function defaultState(models: Model[]): ComparisonState {
  return {
    providers: providers.map((provider) => provider.id),
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

function selection<T extends string>(
  params: URLSearchParams,
  key: string,
  allowed: T[],
  fallback: T[],
): T[] {
  if (!params.has(key)) return [...fallback];
  if (params.get(key) === '') return [];
  const values = [...new Set(params.get(key)!.split(','))].filter((value): value is T =>
    allowed.includes(value as T),
  );
  return values.length ? values : [...fallback];
}

export function readState(search: string, models: Model[], tasks: string[]): ComparisonState {
  const params = new URLSearchParams(search);
  const defaults = defaultState(models);
  const provider = params.get('provider') as Provider;
  return {
    providers: selection(
      params,
      'providers',
      defaults.providers,
      defaults.providers.includes(provider) ? [provider] : defaults.providers,
    ),
    models: selection(params, 'models', defaults.models, defaults.models),
    task: tasks.includes(params.get('task') ?? '') ? params.get('task')! : 'all',
    query: params.get('q') ?? '',
    view: ['runs', 'data'].includes(params.get('view') ?? '') ? 'runs' : 'artwork',
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
    providers:
      state.providers.join(',') === defaults.providers.join(',') ? null : state.providers.join(','),
    models: state.models.join(',') === defaults.models.join(',') ? null : state.models.join(','),
    task: state.task === 'all' ? null : state.task,
    q: state.query || null,
    view: state.view === 'artwork' ? null : state.view,
    zoom: String(state.zoom),
    background: state.background === 'grid' ? null : state.background,
    stats: state.stats ? '1' : null,
    sort: state.sort === 'task' ? null : state.sort,
    direction: state.direction === 'asc' ? null : 'desc',
  };
  url.searchParams.delete('provider');
  for (const [key, value] of Object.entries(values)) {
    if (value === null) url.searchParams.delete(key);
    else url.searchParams.set(key, value);
  }
  return url;
}

export function visibleModels(state: ComparisonState, models: Model[]): Model[] {
  return state.models.flatMap((id) => {
    const model = models.find((model) => model.id === id);
    return model && state.providers.includes(model.provider) ? [model] : [];
  });
}

export function visibleTasks(state: ComparisonState, tasks: string[]): string[] {
  return tasks.filter(
    (task) =>
      (state.task === 'all' || task === state.task) &&
      task.includes(state.query.trim().toLowerCase()),
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
  const ids = new Set(visibleModels(state, models).map((model) => model.id));
  const tasks = new Set(visibleTasks(state, data.tasks));
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
