import { RotateCcw } from 'lucide-react';
import { data, models } from '../data.ts';
import { providers, title, visibleModels } from '../lib/comparison.ts';
import type { ComparisonState, Provider } from '../lib/comparison.ts';
import { ProviderMark } from './ui.tsx';

export function FilterPanel({
  state,
  update,
  reset,
}: {
  state: ComparisonState;
  update: (patch: Partial<ComparisonState>) => void;
  reset: () => void;
}) {
  const toggleProvider = (provider: Provider, checked: boolean) =>
    update({
      providers: checked
        ? providers
            .filter((value) => value.id === provider || state.providers.includes(value.id))
            .map((value) => value.id)
        : state.providers.filter((value) => value !== provider),
    });
  return (
    <div className="filter-panel">
      <div className="filter-heading">
        <h2>Refine comparison</h2>
        <button type="button" className="text-button" onClick={reset} aria-label="Reset filters">
          <RotateCcw size={12} aria-hidden="true" />
          Reset
        </button>
      </div>
      <fieldset>
        <legend>Providers</legend>
        <div className="provider-options">
          {providers.map((provider) => (
            <label className="check-option" key={provider.id}>
              <input
                type="checkbox"
                checked={state.providers.includes(provider.id)}
                onChange={(event) => toggleProvider(provider.id, event.target.checked)}
                data-provider-filter
                value={provider.id}
              />
              <ProviderMark provider={provider.id} small />
              <span>{provider.name}</span>
              <span className="option-count">
                {models.filter((model) => model.provider === provider.id).length}
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      <fieldset>
        <legend>
          Models<span className="count-badge">{visibleModels(state, models).length}</span>
        </legend>
        <div className="model-presets">
          <button
            type="button"
            className="text-button"
            onClick={() => update({ models: models.map((model) => model.id) })}
            aria-label="Select all models"
          >
            All
          </button>
          <span aria-hidden="true">/</span>
          <button
            type="button"
            className="text-button"
            onClick={() => update({ models: [] })}
            aria-label="Deselect all models"
          >
            None
          </button>
        </div>
        <div className="model-options">
          {providers
            .filter((provider) => state.providers.includes(provider.id))
            .map((provider) => (
              <div className="model-group" key={provider.id}>
                <p className="group-label">{provider.name}</p>
                {models
                  .filter((model) => model.provider === provider.id)
                  .map((model) => (
                    <label className="check-option model-option" key={model.id}>
                      <input
                        type="checkbox"
                        checked={state.models.includes(model.id)}
                        data-model-filter
                        value={model.id}
                        onChange={(event) =>
                          update({
                            models: event.target.checked
                              ? [...state.models, model.id]
                              : state.models.filter((id) => id !== model.id),
                          })
                        }
                      />
                      <span>
                        {model.name}
                        {model.effort && <small className="effort">{model.effort}</small>}
                      </span>
                    </label>
                  ))}
              </div>
            ))}
        </div>
        {!state.providers.length && <p className="subtle">Select a provider to see its models.</p>}
      </fieldset>
      <fieldset>
        <legend>Brief</legend>
        <div className="select-wrap">
          <select
            aria-label="Filter by brief"
            value={state.task}
            onChange={(event) => update({ task: event.target.value })}
          >
            <option value="all">All briefs</option>
            {data.tasks.map((task) => (
              <option key={task} value={task}>
                {title(task)}
              </option>
            ))}
          </select>
        </div>
      </fieldset>
    </div>
  );
}
