import source from '../../showcase/runs.json';
import { getModels, runKey } from './lib/comparison.ts';
import type { Dataset } from './lib/comparison.ts';

export const data: Dataset = source;
export const models = getModels(data);
export const runsByKey = new Map(data.runs.map((run) => [runKey(run), run]));
const files = import.meta.glob<string>('../../showcase/tasks/*.txt', {
  query: '?raw',
  import: 'default',
  eager: true,
});
export const briefs = Object.fromEntries(
  data.tasks.map((task) => [task, files[`../../showcase/tasks/${task}.txt`].trim()]),
);
export const asset = (path: string) => `${import.meta.env.BASE_URL}showcase/${path}`;
