import { ArrowUpRight, Maximize2, X } from 'lucide-react';
import { asset, data, runsByKey } from '../data.ts';
import { canvasSize, number, title } from '../lib/comparison.ts';
import type { Model, Run } from '../lib/comparison.ts';
import { ProviderLabel, ProviderMark } from './ui.tsx';
import type { MouseEvent } from 'react';

export function inspectLink(event: MouseEvent<HTMLAnchorElement>, action: () => void) {
  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  action();
}

export function ArtworkTable({
  models,
  tasks,
  zoom,
  stats,
  width,
  briefWidth,
  inspect,
  openBrief,
  hideModel,
}: {
  models: Model[];
  tasks: string[];
  zoom: number;
  stats: boolean;
  width: number;
  briefWidth: number;
  inspect: (run: Run) => void;
  openBrief: (task: string) => void;
  hideModel: (id: string) => void;
}) {
  return (
    <table
      className="artwork-table"
      id="matrix-table"
      style={{ width: briefWidth + width * models.length }}
    >
      <caption className="visually-hidden">
        Original artwork. Each row is a shared brief, each column is a model.
      </caption>
      <colgroup>
        <col style={{ width: briefWidth }} />
        {models.map((model) => (
          <col key={model.id} style={{ width }} />
        ))}
      </colgroup>
      <thead>
        <tr>
          <th scope="col" className="brief-column corner-heading">
            <span>Shared brief</span>
            <small>
              {tasks.length} of {data.tasks.length} briefs
            </small>
          </th>
          {models.map((model) => (
            <th scope="col" key={model.id} data-model-column={model.id}>
              <button
                type="button"
                className="hide-model"
                aria-label={`Hide ${model.name}`}
                onClick={() => hideModel(model.id)}
              >
                <X size={12} aria-hidden="true" />
              </button>
              <div className="column-provider">
                <ProviderMark provider={model.provider} small />
                <ProviderLabel model={model} />
              </div>
              <span className="column-model">{model.name}</span>
              <span className="column-effort">
                {model.effort ? (
                  <span className="effort">{model.effort} effort</span>
                ) : (
                  <span>Standard</span>
                )}
              </span>
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {tasks.map((task) => (
          <tr key={task} data-task-row={task}>
            <th scope="row" className="brief-column">
              <span className="brief-number">
                {String(data.tasks.indexOf(task) + 1).padStart(2, '0')}
              </span>
              <strong>{title(task)}</strong>
              <small>
                {canvasSize(task)} × {canvasSize(task)} px
              </small>
              <a
                href={asset(`tasks/${task}.txt`)}
                data-brief={task}
                onClick={(event) => inspectLink(event, () => openBrief(task))}
              >
                Prompt
                <ArrowUpRight size={11} aria-hidden="true" />
              </a>
            </th>
            {models.map((model) => {
              const run = runsByKey.get(`${model.id}/${task}`)!;
              return (
                <td key={model.id} data-model-column={model.id}>
                  <a
                    className="artwork-stage"
                    href={asset(run.gif)}
                    data-run={`${model.id}/${task}`}
                    aria-label={`Inspect ${title(task)} by ${model.name}${model.effort ? ` (${model.effort})` : ''}`}
                    onClick={(event) => inspectLink(event, () => inspect(run))}
                  >
                    <img
                      src={asset(run.gif)}
                      width={canvasSize(task) * zoom}
                      height={canvasSize(task) * zoom}
                      loading="lazy"
                      alt={`${title(task)} by ${model.name}`}
                    />
                    <span className="inspect-mark">
                      <Maximize2 size={13} aria-hidden="true" />
                    </span>
                  </a>
                  {stats && (
                    <div className="cell-stats">
                      <span>
                        <b>{number(run.tool_calls)}</b> calls · <b>{number(run.looks)}</b> looks
                      </span>
                      <span title={run.tokens === null ? 'Not reported' : 'Reported tokens'}>
                        {number(run.tokens)} tokens
                      </span>
                    </div>
                  )}
                </td>
              );
            })}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
