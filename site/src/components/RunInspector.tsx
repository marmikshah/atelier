import { Download, FileText } from 'lucide-react';
import { asset, briefs, models } from '../data.ts';
import { canvasSize, number, title } from '../lib/comparison.ts';
import type { Run } from '../lib/comparison.ts';
import type { CSSProperties } from 'react';
import { Modal, ProviderLabel, ProviderMark } from './ui.tsx';

export function RunInspector({ run, close }: { run: Run | null; close: () => void }) {
  const model = models.find((model) => model.id === run?.model);
  return (
    <Modal
      open={Boolean(run)}
      onOpenChange={(open) => {
        if (!open) close();
      }}
      title={run ? `${title(run.task)} · ${model!.name}` : 'Run details'}
      label="Original run"
      description="Inspect the original animation, recorded statistics, frozen prompt, and downloadable replay."
      className="run-modal"
    >
      {run && model && (
        <>
          <div className="inspector-top">
            <div className="inspector-stage background-grid">
              <img
                src={asset(run.gif)}
                alt={`${title(run.task)} by ${model.name}`}
                width={canvasSize(run.task) * 6}
                height={canvasSize(run.task) * 6}
                style={{ '--canvas': `${canvasSize(run.task)}px` } as CSSProperties}
              />
            </div>
            <div className="inspector-summary">
              <div className="inspector-model">
                <ProviderMark provider={model.provider} />
                <div>
                  <strong>{model.name}</strong>
                  <ProviderLabel model={model} />
                </div>
              </div>
              <p className="canvas-note">
                {canvasSize(run.task)} × {canvasSize(run.task)} px · {run.frames} frames · 10 fps
              </p>
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
          </div>
          <section className="prompt-section">
            <h3>
              <FileText size={15} aria-hidden="true" />
              Frozen brief
            </h3>
            <pre>{briefs[run.task]}</pre>
          </section>
          <div className="inspector-footer">
            <code>{model.id}</code>
            <div>
              <a className="button" href={asset(run.gif)} download>
                <Download size={14} aria-hidden="true" />
                GIF
              </a>
              <a className="button button-primary" href={asset(run.replay)} download>
                <Download size={14} aria-hidden="true" />
                Download replay
              </a>
            </div>
          </div>
        </>
      )}
    </Modal>
  );
}
