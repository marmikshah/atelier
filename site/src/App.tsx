import { useEffect, useMemo, useRef, useState } from 'react';
import { ArrowUpRight, Check, Download, Link, X } from 'lucide-react';
import { asset, briefs, data, models, runsByKey } from './data.ts';
import {
  defaultState,
  familiesOf,
  filterRuns,
  runsCSV,
  stateURL,
  title,
  visibleModels,
  visibleTasks,
} from './lib/comparison.ts';
import type { Background, Run, Sort, View } from './lib/comparison.ts';
import { useComparisonState } from './hooks/useComparisonState.ts';
import { Command, Lines } from './components/Desk.tsx';
import { RunsTable } from './components/RunsTable.tsx';
import { RunInspector } from './components/RunInspector.tsx';
import { Wall } from './components/Wall.tsx';
import { Brand, Modal } from './components/ui.tsx';

const repository = 'https://github.com/marmikshah/atelier';
const viewTabs: { id: View; label: string }[] = [
  { id: 'artwork', label: 'By brief' },
  { id: 'models', label: 'By model' },
  { id: 'runs', label: 'Data' },
];
const grounds: { id: Background; label: string }[] = [
  { id: 'grid', label: 'Checker' },
  { id: 'light', label: 'Paper' },
  { id: 'dark', label: 'Ink' },
];
const count = (value: number, noun: string) => `${value} ${noun}${value === 1 ? '' : 's'}`;

export function App() {
  const { state, update } = useComparisonState();
  const [methodOpen, setMethodOpen] = useState(window.location.hash === '#about');
  const [brief, setBrief] = useState<string | null>(null);
  const [inspected, setInspected] = useState<Run | null>(null);
  const [share, setShare] = useState('');
  const [copied, setCopied] = useState(false);
  const shareInput = useRef<HTMLInputElement>(null);
  const commandInput = useRef<HTMLInputElement>(null);
  const selectedModels = useMemo(() => visibleModels(state, models, data.tasks), [state]);
  const selectedTasks = useMemo(() => visibleTasks(state, models, data.tasks), [state]);
  const runs = useMemo(() => filterRuns(data, state, models), [state]);
  // The pieces in the order they hang, so the inspector can step along the wall.
  const hung = useMemo(() => {
    if (state.view === 'runs') return runs;
    const pair = (model: string, task: string) => runsByKey.get(`${model}/${task}`)!;
    if (state.view === 'artwork')
      return selectedTasks.flatMap((task) => selectedModels.map((model) => pair(model.id, task)));
    return familiesOf(selectedModels).flatMap((family) =>
      selectedModels
        .filter((model) => model.family === family)
        .flatMap((model) => selectedTasks.map((task) => pair(model.id, task))),
    );
  }, [state.view, runs, selectedModels, selectedTasks]);

  const reset = () => {
    const defaults = defaultState(models);
    update({ families: [], efforts: [], models: defaults.models, task: 'all', query: '' });
  };
  const filtered =
    state.families.length > 0 ||
    state.efforts.length > 0 ||
    state.models.length !== models.length ||
    state.task !== 'all' ||
    state.query !== '';
  const sort = (key: Sort) =>
    update({
      sort: key,
      direction:
        state.sort === key
          ? state.direction === 'asc'
            ? 'desc'
            : 'asc'
          : ['calls', 'looks', 'tokens'].includes(key)
            ? 'desc'
            : 'asc',
    });

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2500);
    return () => clearTimeout(timer);
  }, [copied]);
  useEffect(() => {
    if (share) {
      shareInput.current?.focus();
      shareInput.current?.select();
    }
  }, [share]);
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === 'k' &&
        !methodOpen &&
        !brief &&
        !inspected
      ) {
        event.preventDefault();
        commandInput.current?.focus();
        commandInput.current?.select();
      }
    };
    window.addEventListener('keydown', shortcut);
    return () => window.removeEventListener('keydown', shortcut);
  }, [methodOpen, brief, inspected]);

  async function shareView() {
    const url = stateURL(state, models, window.location.href);
    url.hash = 'gallery';
    window.history.replaceState(null, '', url);
    setShare('');
    try {
      await navigator.clipboard.writeText(url.href);
      setCopied(true);
    } catch {
      setShare(url.href);
    }
  }
  function exportCSV() {
    const url = URL.createObjectURL(
      new Blob([runsCSV(runs, window.location.href)], { type: 'text/csv;charset=utf-8' }),
    );
    const link = document.createElement('a');
    link.href = url;
    link.download = 'atelier-runs.csv';
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  return (
    <>
      <a className="skip-link" href="#gallery">
        Skip to the gallery
      </a>
      <header className="masthead">
        <Brand />
        <dl className="tally" aria-label="Exhibition totals">
          <div>
            <dt>Models</dt>
            <dd>{models.length}</dd>
          </div>
          <div>
            <dt>Briefs</dt>
            <dd>{data.tasks.length}</dd>
          </div>
          <div>
            <dt>Runs</dt>
            <dd>{data.runs.length}</dd>
          </div>
        </dl>
        <nav aria-label="Site">
          <button type="button" className="text-button" onClick={() => setMethodOpen(true)}>
            About
          </button>
          <a className="text-button" href={repository}>
            GitHub
            <ArrowUpRight size={13} aria-hidden="true" />
          </a>
        </nav>
      </header>
      <main>
        <h1 className="tagline" id="page-title">
          Same canvas. Same briefs. <em>Every model draws its own.</em>
        </h1>
        <section className="gallery" id="gallery" aria-label="Gallery">
          <div className="hud">
            <div className="hud-row">
              <Command state={state} update={update} input={commandInput} />
              <div className="view-switch" role="group" aria-label="Arrangement">
                {viewTabs.map(({ id, label }) => (
                  <button
                    type="button"
                    key={id}
                    data-view={id}
                    aria-pressed={state.view === id}
                    onClick={() => update({ view: id })}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>
            <div className="hud-row">
              <Lines state={state} update={update} />
              <div className="hud-tools">
                {state.view !== 'runs' && (
                  <>
                    <label className="select-control">
                      Zoom
                      <select
                        id="pixel-zoom"
                        aria-label="Pixel zoom"
                        value={state.zoom}
                        onChange={(event) => update({ zoom: Number(event.target.value) })}
                      >
                        {[2, 3, 4, 6].map((zoom) => (
                          <option key={zoom} value={zoom}>
                            {zoom}×
                          </option>
                        ))}
                      </select>
                    </label>
                    <div
                      className="ground-controls"
                      role="group"
                      aria-label="Ground behind artwork"
                    >
                      {grounds.map((ground) => (
                        <button
                          type="button"
                          key={ground.id}
                          data-background={ground.id}
                          aria-label={`${ground.label} ground`}
                          title={`${ground.label} ground`}
                          aria-pressed={state.background === ground.id}
                          onClick={() => update({ background: ground.id })}
                        >
                          <span className={`ground-swatch ground-${ground.id}`} />
                        </button>
                      ))}
                    </div>
                    <label className="check-control">
                      <input
                        type="checkbox"
                        id="show-stats"
                        checked={state.stats}
                        onChange={(event) => update({ stats: event.target.checked })}
                      />
                      Stats
                    </label>
                  </>
                )}
                <button
                  type="button"
                  className="tool-button"
                  id="share-link"
                  aria-label={copied ? 'Link copied' : 'Share this view'}
                  title="Copy a link to this view"
                  onClick={shareView}
                >
                  {copied ? (
                    <Check size={14} aria-hidden="true" />
                  ) : (
                    <Link size={14} aria-hidden="true" />
                  )}
                  <span>{copied ? 'Copied' : 'Share'}</span>
                </button>
                <button
                  type="button"
                  className="tool-button"
                  id="export-csv"
                  title="Download the shown runs as CSV"
                  disabled={!runs.length}
                  onClick={exportCSV}
                >
                  <Download size={14} aria-hidden="true" />
                  <span>CSV</span>
                </button>
              </div>
            </div>
          </div>
          <p className="result-count" id="result-count" role="status">
            <strong>{count(runs.length, state.view === 'runs' ? 'run' : 'piece')}</strong>
            <span>
              {count(selectedModels.length, 'model')} × {count(selectedTasks.length, 'brief')}
            </span>
            {filtered && (
              <button type="button" className="text-button" onClick={reset}>
                Reset
              </button>
            )}
          </p>
          {share && (
            <div className="share-fallback">
              <label htmlFor="share-url">Copy this link to share your view</label>
              <input ref={shareInput} id="share-url" value={share} readOnly />
              <button
                type="button"
                className="icon-button"
                onClick={() => setShare('')}
                aria-label="Dismiss share link"
              >
                <X size={15} aria-hidden="true" />
              </button>
            </div>
          )}
          <span role="status" className="visually-hidden">
            {copied ? 'View link copied to clipboard' : ''}
          </span>
          {runs.length ? (
            <div
              className={`wall-scroll ground-${state.background}`}
              tabIndex={0}
              aria-label="Scrollable gallery"
            >
              {state.view === 'runs' ? (
                <RunsTable
                  runs={runs}
                  models={models}
                  state={state}
                  sort={sort}
                  inspect={setInspected}
                />
              ) : (
                <Wall
                  by={state.view === 'artwork' ? 'brief' : 'model'}
                  models={selectedModels}
                  tasks={selectedTasks}
                  zoom={state.zoom}
                  stats={state.stats}
                  inspect={setInspected}
                  openBrief={setBrief}
                  hideModel={(id) =>
                    update({ models: state.models.filter((model) => model !== id) })
                  }
                />
              )}
            </div>
          ) : (
            <div className="empty-state">
              <h2>Nothing here</h2>
              <p>No runs match this selection.</p>
              <button type="button" className="button button-primary" onClick={reset}>
                Show everything
              </button>
            </div>
          )}
          <p className="wall-note">
            {state.view === 'runs' || state.stats
              ? '— means the client did not report a total. '
              : 'Select any piece to see its run, brief, and replay. '}
            Every piece is ten frames, looping once a second.
          </p>
        </section>
      </main>
      <footer className="colophon">
        <p>
          New runs use xhigh effort. Earlier effort levels and counts are not directly comparable.{' '}
          <button type="button" className="text-button" onClick={() => setMethodOpen(true)}>
            Read the methodology
          </button>
        </p>
        <p>
          Atelier is an experiment in art made through tools.{' '}
          <a className="text-button" href={`${repository}/tree/master/showcase`}>
            Source data &amp; replays
            <ArrowUpRight size={12} aria-hidden="true" />
          </a>
        </p>
      </footer>
      <RunInspector
        run={inspected}
        runs={hung}
        inspect={setInspected}
        close={() => setInspected(null)}
      />
      <Modal
        open={Boolean(brief)}
        onOpenChange={(open) => {
          if (!open) setBrief(null);
        }}
        title={brief ? title(brief) : 'Frozen brief'}
        label="The brief every model received"
        description="The exact frozen prompt used for this artwork."
      >
        {brief && (
          <div className="brief-modal">
            <blockquote>{briefs[brief]}</blockquote>
            <a className="button" href={asset(`tasks/${brief}.txt`)} download>
              <Download size={14} aria-hidden="true" />
              Download brief
            </a>
          </div>
        )}
      </Modal>
      <Modal
        open={methodOpen}
        onOpenChange={setMethodOpen}
        title="How this was made"
        label="About"
        description="The original methodology, server provenance, and reproduction notes."
        className="method-modal"
      >
        <div className="method-body">
          <p className="method-lede">
            Every model receives the same ten frozen briefs and creates its artwork through
            Atelier’s editing tools. The gallery shows the original animations, and each one can be
            rebuilt from its replay journal.
          </p>
          <section>
            <h3>Reasoning effort</h3>
            <p>
              From the Opus 5.5, Sonnet 5.5, and Haiku 5.5 runs onward, every model is tested at
              xhigh reasoning effort. Some earlier runs used max effort and others did not record an
              effort level, so they are not directly comparable with xhigh runs. xhigh runs for the
              remaining models will be added as access to them becomes available. Tool-call and
              token counts also follow each client’s reporting method; they are not an efficiency
              ranking.
            </p>
          </section>
          <section>
            <h3>How the runs were collected</h3>
            <p>{data.method}</p>
          </section>
          <section>
            <h3>Editor &amp; client provenance</h3>
            <p>{data.server}</p>
          </section>
          <section>
            <h3>Reproducing the artwork</h3>
            <p>{data.verified}</p>
            <p>
              Animations loop independently in the browser. Pixel scale uses whole-number
              magnification.
            </p>
          </section>
          <div className="method-download">
            <a className="button button-primary" href={asset('runs.json')} download>
              <Download size={14} aria-hidden="true" />
              Download original data
            </a>
            <a className="text-button" href={repository}>
              Explore Atelier
              <ArrowUpRight size={13} aria-hidden="true" />
            </a>
          </div>
        </div>
      </Modal>
    </>
  );
}
