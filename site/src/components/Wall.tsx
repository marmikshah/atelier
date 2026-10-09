import { ArrowUpRight, X } from 'lucide-react';
import type { CSSProperties } from 'react';
import { asset, data, runsByKey } from '../data.ts';
import { canvasSize, familiesOf, familyName, number, title } from '../lib/comparison.ts';
import type { Model, Run } from '../lib/comparison.ts';
import { Placard, inspectLink } from './ui.tsx';

interface WallProps {
  /** Hang briefs as rows and models as columns, or the reverse grouped by model line. */
  by: 'brief' | 'model';
  models: Model[];
  tasks: string[];
  zoom: number;
  stats: boolean;
  inspect: (run: Run) => void;
  openBrief: (task: string) => void;
  hideModel: (id: string) => void;
}

const plate = (task: string) => String(data.tasks.indexOf(task) + 1).padStart(2, '0');

function Piece({
  model,
  task,
  zoom,
  stats,
  inspect,
}: Pick<WallProps, 'zoom' | 'stats' | 'inspect'> & { model: Model; task: string }) {
  const run = runsByKey.get(`${model.id}/${task}`)!;
  return (
    <td>
      <a
        className="piece"
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
      </a>
      {stats && (
        <span className="piece-stats">
          <span>
            <b>{number(run.tool_calls)}</b> calls
          </span>
          <span>
            <b>{number(run.looks)}</b> looks
          </span>
          <span title={run.tokens === null ? 'Not reported' : 'Reported tokens'}>
            <b>{number(run.tokens)}</b> tok
          </span>
        </span>
      )}
    </td>
  );
}

function BriefLabel({ task, openBrief }: { task: string; openBrief: (task: string) => void }) {
  return (
    <>
      <span className="plate">No. {plate(task)}</span>
      <strong className="brief-title">{title(task)}</strong>
      <a
        className="brief-link"
        href={asset(`tasks/${task}.txt`)}
        data-brief={task}
        onClick={(event) => inspectLink(event, () => openBrief(task))}
      >
        Read brief
        <ArrowUpRight size={11} aria-hidden="true" />
      </a>
    </>
  );
}

function ModelLabel({ model, hideModel }: { model: Model; hideModel: (id: string) => void }) {
  return (
    <>
      <Placard model={model} />
      <button
        type="button"
        className="hide-model"
        aria-label={`Hide ${model.name}`}
        title={`Hide ${model.name}`}
        onClick={() => hideModel(model.id)}
      >
        <X size={12} aria-hidden="true" />
      </button>
    </>
  );
}

export function Wall({ by, models, tasks, zoom, stats, inspect, openBrief, hideModel }: WallProps) {
  const style = { '--cell': `${48 * zoom + 8}px` } as CSSProperties;
  if (by === 'brief')
    return (
      <table className="wall wall-by-brief" id="matrix-table" style={style}>
        <caption className="visually-hidden">
          Original artwork. Each row is a shared brief, each column is a model.
        </caption>
        <thead>
          <tr>
            <th scope="col" className="wall-corner">
              <span className="plate">Brief ↓ · Model →</span>
            </th>
            {models.map((model) => (
              <th scope="col" key={model.id} data-model-column={model.id}>
                <ModelLabel model={model} hideModel={hideModel} />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {tasks.map((task) => (
            <tr key={task} data-task-row={task}>
              <th scope="row">
                <BriefLabel task={task} openBrief={openBrief} />
              </th>
              {models.map((model) => (
                <Piece key={model.id} {...{ model, task, zoom, stats, inspect }} />
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    );
  return (
    <table className="wall wall-by-model" id="matrix-table" style={style}>
      <caption className="visually-hidden">
        Original artwork. Each row is a model, grouped by model line; each column is a shared brief.
      </caption>
      <thead>
        <tr>
          <th scope="col" className="wall-corner">
            <span className="plate">Model ↓ · Brief →</span>
          </th>
          {tasks.map((task) => (
            <th scope="col" key={task}>
              <BriefLabel task={task} openBrief={openBrief} />
            </th>
          ))}
        </tr>
      </thead>
      {familiesOf(models).map((family) => {
        const line = models.filter((model) => model.family === family);
        return (
          <tbody key={family} data-family={family}>
            <tr className="family-row">
              <th scope="rowgroup" colSpan={tasks.length + 1}>
                <span className="family-name">{familyName(family)}</span>
                <span className="plate">
                  {line[0].vendor} · {line.length} {line.length === 1 ? 'model' : 'models'}
                </span>
              </th>
            </tr>
            {line.map((model) => (
              <tr key={model.id} data-model-row={model.id}>
                <th scope="row">
                  <ModelLabel model={model} hideModel={hideModel} />
                </th>
                {tasks.map((task) => (
                  <Piece key={task} {...{ model, task, zoom, stats, inspect }} />
                ))}
              </tr>
            ))}
          </tbody>
        );
      })}
    </table>
  );
}
