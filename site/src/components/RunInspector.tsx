import { ChevronLeft, ChevronRight, Download } from 'lucide-react';
import { asset, briefs, data, models } from '../data.ts';
import { canvasSize, number, title } from '../lib/comparison.ts';
import type { Run } from '../lib/comparison.ts';
import { Modal, Placard } from './ui.tsx';

export function RunInspector({
  run,
  runs,
  inspect,
  close,
}: {
  run: Run | null;
  /** The pieces currently on the wall, in hanging order, for stepping between them. */
  runs: Run[];
  inspect: (run: Run) => void;
  close: () => void;
}) {
  const model = models.find((model) => model.id === run?.model);
  const index = run ? runs.indexOf(run) : -1;
  const step = (offset: number) => inspect(runs[(index + offset + runs.length) % runs.length]);
  return (
    <Modal
      open={Boolean(run)}
      onOpenChange={(open) => {
        if (!open) close();
      }}
      title={run ? title(run.task) : 'Run details'}
      label={run ? `No. ${String(data.tasks.indexOf(run.task) + 1).padStart(2, '0')}` : undefined}
      description="Inspect the original animation, recorded statistics, frozen prompt, and downloadable replay."
      className="run-modal"
      onKeyDown={(event) => {
        if (index < 0 || runs.length < 2) return;
        if (event.key === 'ArrowLeft') step(-1);
        if (event.key === 'ArrowRight') step(1);
      }}
    >
      {run && model && (
        <div className="inspector">
          <div className="inspector-stage">
            <img
              src={asset(run.gif)}
              alt={`${title(run.task)} by ${model.name}`}
              width={Math.floor(300 / canvasSize(run.task)) * canvasSize(run.task)}
              height={Math.floor(300 / canvasSize(run.task)) * canvasSize(run.task)}
            />
          </div>
          <div className="inspector-label">
            <Placard model={model} />
            <p className="medium">
              Pixels on transparent ground · {canvasSize(run.task)} × {canvasSize(run.task)} px ·{' '}
              {run.frames} frames at 10 fps
            </p>
            <blockquote>{briefs[run.task]}</blockquote>
            <dl className="run-metrics">
              <div>
                <dt>Tool calls</dt>
                <dd>{number(run.tool_calls)}</dd>
              </div>
              <div>
                <dt>Looks</dt>
                <dd>{number(run.looks)}</dd>
              </div>
              <div>
                <dt>Reported tokens</dt>
                <dd className={run.tokens === null ? 'unreported' : ''}>
                  {run.tokens === null ? 'Not reported' : number(run.tokens)}
                </dd>
              </div>
            </dl>
            <p className="subtle">Counts follow each client’s reporting method.</p>
          </div>
          <div className="inspector-footer">
            {index >= 0 && runs.length > 1 && (
              <div className="inspector-steps">
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Previous piece"
                  onClick={() => step(-1)}
                >
                  <ChevronLeft size={16} aria-hidden="true" />
                </button>
                <span>
                  {index + 1} / {runs.length}
                </span>
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Next piece"
                  onClick={() => step(1)}
                >
                  <ChevronRight size={16} aria-hidden="true" />
                </button>
              </div>
            )}
            <code>{model.id}</code>
            <a className="button" href={asset(run.gif)} download>
              <Download size={14} aria-hidden="true" />
              GIF
            </a>
            <a className="button button-primary" href={asset(run.replay)} download>
              <Download size={14} aria-hidden="true" />
              Replay
            </a>
          </div>
        </div>
      )}
    </Modal>
  );
}
