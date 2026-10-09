import { X } from 'lucide-react';
import type { RefObject } from 'react';
import { data, models } from '../data.ts';
import { familiesOf, familyName, parseCommand } from '../lib/comparison.ts';
import type { ComparisonState } from '../lib/comparison.ts';

type Update = (patch: Partial<ComparisonState>, history?: 'push' | 'replace') => void;
const families = familiesOf(models);
const toggle = (values: string[], value: string) =>
  values.includes(value) ? values.filter((item) => item !== value) : [...values, value];

/** Type a model line, version, effort, provider, or brief to show it. */
export function Command({
  state,
  update,
  input,
}: {
  state: ComparisonState;
  update: Update;
  input: RefObject<HTMLInputElement | null>;
}) {
  const unknown = parseCommand(state.query, models, data.tasks).unknown;
  return (
    <div className={`command${unknown.length ? ' command-unknown' : ''}`}>
      <span className="command-prompt" aria-hidden="true">
        &gt;
      </span>
      <input
        ref={input}
        type="search"
        id="command"
        aria-label="Show models and briefs by name"
        aria-describedby="command-hint"
        placeholder="haiku / opus xhigh / sonnet, gpt / anthropic cat"
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
        <kbd>{navigator.platform.includes('Mac') ? '⌘K' : 'Ctrl K'}</kbd>
      )}
      <p className="command-hint" id="command-hint" role="status">
        {unknown.length > 0 && (
          <>
            No model or brief starts with <b>{unknown.join(', ')}</b>
          </>
        )}
      </p>
    </div>
  );
}

/** One-click model lines, plus the xhigh-only switch. */
export function Lines({ state, update }: { state: ComparisonState; update: Update }) {
  return (
    <div className="lines" role="group" aria-label="Model lines">
      <button
        type="button"
        className="chip"
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
            className={`chip pigment ${line[0].provider}`}
            data-family={family}
            aria-pressed={state.families.includes(family)}
            title={`${line[0].vendor}: ${line.map((model) => model.name).join(', ')}`}
            onClick={() => update({ families: toggle(state.families, family) })}
          >
            {familyName(family)}
            <small>{line.length}</small>
          </button>
        );
      })}
      <button
        type="button"
        className="chip chip-effort"
        data-effort="xhigh"
        aria-pressed={state.efforts.join() === 'xhigh'}
        title="Show only runs at xhigh effort, the level used for all new runs"
        onClick={() => update({ efforts: state.efforts.join() === 'xhigh' ? [] : ['xhigh'] })}
      >
        xhigh only
      </button>
    </div>
  );
}
