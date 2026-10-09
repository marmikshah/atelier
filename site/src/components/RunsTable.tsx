import { ArrowDown, ArrowUp, ArrowUpDown, Download } from 'lucide-react';
import { asset } from '../data.ts';
import { canvasSize, number, runKey, title } from '../lib/comparison.ts';
import type { ComparisonState, Model, Run, Sort } from '../lib/comparison.ts';
import { effortKey, effortName } from '../lib/comparison.ts';
import { inspectLink } from './ui.tsx';

const columns: { key?: Sort; label: string; numeric?: boolean }[] = [
  { key: 'task', label: 'Brief' },
  { label: 'Artwork' },
  { key: 'model', label: 'Model' },
  { key: 'provider', label: 'Provider' },
  { key: 'calls', label: 'Calls', numeric: true },
  { key: 'looks', label: 'Looks', numeric: true },
  { key: 'tokens', label: 'Reported tokens', numeric: true },
  { label: 'Replay' },
];

export function RunsTable({
  runs,
  models,
  state,
  sort,
  inspect,
}: {
  runs: Run[];
  models: Model[];
  state: ComparisonState;
  sort: (key: Sort) => void;
  inspect: (run: Run) => void;
}) {
  return (
    <table className="ledger" id="runs-table">
      <caption className="visually-hidden">
        Original recorded run data. Unreported token totals are shown as a dash.
      </caption>
      <thead>
        <tr>
          {columns.map((column) => (
            <th
              key={column.label}
              scope="col"
              className={column.numeric ? 'numeric' : ''}
              aria-sort={
                column.key
                  ? state.sort === column.key
                    ? state.direction === 'asc'
                      ? 'ascending'
                      : 'descending'
                    : 'none'
                  : undefined
              }
            >
              {column.key ? (
                <button type="button" onClick={() => sort(column.key!)} data-sort={column.key}>
                  {column.label}
                  {state.sort === column.key ? (
                    state.direction === 'asc' ? (
                      <ArrowUp size={13} aria-hidden="true" />
                    ) : (
                      <ArrowDown size={13} aria-hidden="true" />
                    )
                  ) : (
                    <ArrowUpDown size={12} aria-hidden="true" />
                  )}
                </button>
              ) : (
                column.label
              )}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {runs.map((run) => {
          const model = models.find((model) => model.id === run.model)!;
          return (
            <tr key={runKey(run)} data-run-row={runKey(run)}>
              <th scope="row">{title(run.task)}</th>
              <td>
                <a
                  className="run-preview"
                  href={asset(run.gif)}
                  data-run={runKey(run)}
                  aria-label={`Inspect ${title(run.task)} by ${model.name}`}
                  onClick={(event) => inspectLink(event, () => inspect(run))}
                >
                  <img
                    src={asset(run.gif)}
                    width={canvasSize(run.task)}
                    height={canvasSize(run.task)}
                    loading="lazy"
                    alt={`${title(run.task)} by ${model.name}`}
                  />
                </a>
              </td>
              <td>
                <span className="data-model-name">{model.name}</span>
                <span className={`effort effort-${effortKey(model)}`}>
                  {effortName(effortKey(model))}
                </span>
              </td>
              <td>
                <span className={`pigment ${model.provider}`}>{model.vendor}</span>
              </td>
              <td className="numeric">{number(run.tool_calls)}</td>
              <td className="numeric">{number(run.looks)}</td>
              <td
                className="numeric"
                data-token-total={run.tokens ?? ''}
                title={run.tokens === null ? 'Not reported' : 'Reported tokens'}
              >
                {number(run.tokens)}
              </td>
              <td>
                <a
                  className="replay-link"
                  href={asset(run.replay)}
                  download
                  aria-label={`Download ${run.task} replay by ${model.name}`}
                >
                  <Download size={13} aria-hidden="true" />
                  JSONL
                </a>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
