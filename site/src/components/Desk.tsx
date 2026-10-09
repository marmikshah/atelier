import { CornerDownLeft, X } from 'lucide-react';
import type { RefObject } from 'react';
import { data, models } from '../data.ts';
import {
  effortKey,
  effortName,
  effortsOf,
  familiesOf,
  familyName,
  parseCommand,
  title,
} from '../lib/comparison.ts';
import type { ComparisonState } from '../lib/comparison.ts';

const families = familiesOf(models);
const efforts = effortsOf(models);
const toggle = (values: string[], value: string) =>
  values.includes(value) ? values.filter((item) => item !== value) : [...values, value];

/** The curator's desk: a typed command plus one-click model lines, efforts, and briefs. */
export function Desk({
  state,
  update,
  reset,
  input,
}: {
  state: ComparisonState;
  update: (patch: Partial<ComparisonState>, history?: 'push' | 'replace') => void;
  reset: () => void;
  input: RefObject<HTMLInputElement | null>;
}) {
  const command = parseCommand(state.query, models, data.tasks);
  const hidden = models.length - state.models.length;
  const latest = () =>
    update({
      families: [],
      efforts: [],
      models: [
        ...new Set(models.map((model) => models.find((m) => m.provider === model.provider)!.id)),
      ],
    });
  return (
    <div className="desk">
      <div className="command">
        <span className="command-prompt" aria-hidden="true">
          ›
        </span>
        <input
          ref={input}
          type="search"
          id="command"
          aria-label="Show models and briefs by name"
          aria-describedby="command-hint"
          placeholder="Show me…  haiku  ·  opus xhigh  ·  sonnet, gpt  ·  anthropic cat"
          autoComplete="off"
          spellCheck={false}
          value={state.query}
          onChange={(event) => update({ query: event.target.value }, 'replace')}
        />
        {state.query ? (
          <button
            type="button"
            className="icon-button"
            aria-label="Clear command"
            onClick={() => update({ query: '' })}
          >
            <X size={14} aria-hidden="true" />
          </button>
        ) : (
          <kbd>{navigator.platform.includes('Mac') ? '⌘ K' : 'Ctrl K'}</kbd>
        )}
      </div>
      <p className="command-hint" id="command-hint" role="status">
        {command.unknown.length ? (
          <>
            Nothing here is called <b>{command.unknown.join(', ')}</b>. Try a model line, a version,
            an effort, a provider, or a brief.
          </>
        ) : (
          <>
            <CornerDownLeft size={11} aria-hidden="true" />
            Name a model line, version, effort, provider, or brief. Commas add another group.
          </>
        )}
      </p>
      <div className="swatches">
        <div className="swatch-group" role="group" aria-label="Model lines">
          <span className="plate">Lines</span>
          <button
            type="button"
            className="swatch"
            aria-pressed={!state.families.length}
            onClick={() => update({ families: [] })}
          >
            All
          </button>
          {families.map((family) => {
            const line = models.filter((model) => model.family === family);
            return (
              <button
                type="button"
                key={family}
                className={`swatch pigment ${line[0].provider}`}
                data-family={family}
                aria-pressed={state.families.includes(family)}
                title={`${line[0].vendor} · ${line.map((model) => model.name).join(', ')}`}
                onClick={() => update({ families: toggle(state.families, family) })}
              >
                {familyName(family)}
                <small>{line.length}</small>
              </button>
            );
          })}
        </div>
        <div className="swatch-group" role="group" aria-label="Reasoning effort">
          <span className="plate">Effort</span>
          <button
            type="button"
            className="swatch"
            aria-pressed={!state.efforts.length}
            onClick={() => update({ efforts: [] })}
          >
            Any
          </button>
          {efforts.map((effort) => (
            <button
              type="button"
              key={effort}
              className="swatch"
              data-effort={effort}
              aria-pressed={state.efforts.includes(effort)}
              onClick={() => update({ efforts: toggle(state.efforts, effort) })}
            >
              {effortName(effort)}
              <small>{models.filter((model) => effortKey(model) === effort).length}</small>
            </button>
          ))}
        </div>
        <div className="swatch-group" role="group" aria-label="Briefs">
          <span className="plate">Brief</span>
          <button
            type="button"
            className="swatch"
            aria-pressed={state.task === 'all'}
            onClick={() => update({ task: 'all' })}
          >
            All ten
          </button>
          {data.tasks.map((task) => (
            <button
              type="button"
              key={task}
              className="swatch"
              data-task={task}
              aria-pressed={state.task === task}
              onClick={() => update({ task: state.task === task ? 'all' : task })}
            >
              {title(task)}
            </button>
          ))}
        </div>
        <div className="swatch-group swatch-actions">
          <button
            type="button"
            className="text-button"
            title="The last recorded model from each provider"
            onClick={latest}
          >
            Latest from each provider
          </button>
          {hidden > 0 && (
            <button
              type="button"
              className="text-button"
              onClick={() => update({ models: models.map((model) => model.id) })}
            >
              Restore {hidden} hidden {hidden === 1 ? 'model' : 'models'}
            </button>
          )}
          <button type="button" className="text-button" onClick={reset}>
            Reset
          </button>
        </div>
      </div>
    </div>
  );
}
