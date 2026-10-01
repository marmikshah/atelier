import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import {
  ArrowUpRight,
  Check,
  CheckCheck,
  ChevronLeft,
  ChevronRight,
  Download,
  FileText,
  Grid2X2,
  Info,
  Link,
  Palette,
  Search,
  SlidersHorizontal,
  Table2,
  X,
} from 'lucide-react';
import { asset, briefs, data, models } from './data.ts';
import {
  defaultState,
  filterRuns,
  providers,
  runsCSV,
  stateURL,
  title,
  visibleModels,
  visibleTasks,
} from './lib/comparison.ts';
import type { Background, Run, Sort } from './lib/comparison.ts';
import { useComparisonState } from './hooks/useComparisonState.ts';
import { ArtworkTable } from './components/ArtworkTable.tsx';
import { FilterPanel } from './components/FilterPanel.tsx';
import { RunsTable } from './components/RunsTable.tsx';
import { RunInspector } from './components/RunInspector.tsx';
import { Brand, Modal } from './components/ui.tsx';

const repository = 'https://github.com/marmikshah/atelier';

export function App() {
  const { state, update } = useComparisonState();
  const [filterOpen, setFilterOpen] = useState(false);
  const [methodOpen, setMethodOpen] = useState(window.location.hash === '#about');
  const [brief, setBrief] = useState<string | null>(null);
  const [inspected, setInspected] = useState<Run | null>(null);
  const [share, setShare] = useState('');
  const [copied, setCopied] = useState(false);
  const [mobile, setMobile] = useState(window.innerWidth <= 600);
  const [scroll, setScroll] = useState({ width: 0, previous: false, next: false });
  const surface = useRef<HTMLDivElement>(null);
  const shareInput = useRef<HTMLInputElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const selectedModels = useMemo(() => visibleModels(state, models), [state]);
  const selectedTasks = useMemo(() => visibleTasks(state, data.tasks), [state]);
  const runs = useMemo(() => filterRuns(data, state, models), [state]);
  const briefWidth = mobile ? 94 : 126;
  const columnWidth = Math.max(
    112,
    48 * state.zoom + 16,
    (scroll.width - briefWidth) / (selectedModels.length || 1),
  );

  const reset = () => {
    const defaults = defaultState(models);
    update({ providers: defaults.providers, models: defaults.models, task: 'all', query: '' });
  };
  const latest = () =>
    update({
      models: state.providers.map(
        (provider) => models.find((model) => model.provider === provider)!.id,
      ),
    });
  const allModels = () => update({ models: models.map((model) => model.id) });
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

  const measure = () => {
    const element = surface.current;
    if (!element) return;
    const next = {
      width: element.clientWidth,
      previous: element.scrollLeft > 1,
      next: element.scrollLeft + element.clientWidth < element.scrollWidth - 1,
    };
    setScroll((current) =>
      current.width === next.width &&
      current.previous === next.previous &&
      current.next === next.next
        ? current
        : next,
    );
  };
  useLayoutEffect(measure, [
    state.zoom,
    state.view,
    columnWidth,
    selectedModels.length,
    selectedTasks.length,
  ]);
  useEffect(() => {
    const observer = new ResizeObserver(measure);
    if (surface.current) observer.observe(surface.current);
    const resize = () => {
      setMobile(window.innerWidth <= 600);
      if (window.innerWidth > 1100) setFilterOpen(false);
    };
    window.addEventListener('resize', resize);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', resize);
    };
  }, []);
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
        !filterOpen &&
        !methodOpen &&
        !brief &&
        !inspected
      ) {
        event.preventDefault();
        searchInput.current?.focus();
      }
    };
    window.addEventListener('keydown', shortcut);
    return () => window.removeEventListener('keydown', shortcut);
  }, [filterOpen, methodOpen, brief, inspected]);

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

  const chips: { label: string; clear: () => void }[] = [];
  if (state.providers.length !== providers.length)
    chips.push({
      label:
        state.providers
          .map((id) => providers.find((provider) => provider.id === id)!.name)
          .join(', ') || 'No providers',
      clear: () => update({ providers: providers.map((provider) => provider.id) }),
    });
  if (state.models.length !== models.length)
    chips.push({ label: `${selectedModels.length} selected models`, clear: allModels });
  if (state.task !== 'all')
    chips.push({ label: title(state.task), clear: () => update({ task: 'all' }) });
  if (state.query) chips.push({ label: `“${state.query}”`, clear: () => update({ query: '' }) });

  return (
    <>
      <a className="skip-link" href="#gallery">
        Skip to comparison
      </a>
      <div className="app-shell">
        <aside className="sidebar" aria-label="Comparison navigation and filters">
          <div className="sidebar-brand">
            <Brand />
            <p>The pixel art experiment</p>
          </div>
          <nav className="sidebar-nav" aria-label="Site">
            <a href="#gallery" className="nav-active" aria-current="page">
              <Grid2X2 size={16} aria-hidden="true" />
              Model comparison<span className="count-badge">{models.length}</span>
            </a>
            <button type="button" onClick={() => setMethodOpen(true)}>
              <FileText size={16} aria-hidden="true" />
              About the experiment
              <ArrowUpRight size={13} aria-hidden="true" />
            </button>
          </nav>
          <div className="sidebar-filters">
            <FilterPanel state={state} update={update} reset={reset} />
          </div>
          <div className="sidebar-footer">
            <span className="verified-dot" />
            <div>
              <strong>Created through tools</strong>
              <p>Shared prompts. Original pixels.</p>
            </div>
          </div>
        </aside>
        <div className="main-shell">
          <header className="topbar">
            <div className="mobile-brand">
              <Brand />
            </div>
            <div className="breadcrumb">
              <span>Showcase</span>
              <ChevronRight size={13} aria-hidden="true" />
              <strong>Model comparison</strong>
            </div>
            <div className="topbar-actions">
              <span className="verified-label">
                <CheckCheck size={15} aria-hidden="true" />
                Reproducible runs
              </span>
              <a href={repository} className="source-link">
                GitHub
                <ArrowUpRight size={13} aria-hidden="true" />
              </a>
            </div>
          </header>
          <main className="main-content">
            <section className="page-intro" aria-labelledby="page-title">
              <div>
                <div className="eyebrow">
                  <span />
                  THE MODEL SHOWCASE
                </div>
                <h1 id="page-title">
                  Compare every pixel<span>.</span>
                </h1>
                <p>Same canvas. Same briefs. A different imagination in every column.</p>
              </div>
              <dl className="experiment-totals">
                <div>
                  <dt>Models</dt>
                  <dd>
                    {models.length}
                    <span>across {providers.length} providers</span>
                  </dd>
                </div>
                <div>
                  <dt>Briefs</dt>
                  <dd>
                    {data.tasks.length}
                    <span>identical prompts</span>
                  </dd>
                </div>
                <div>
                  <dt>Runs</dt>
                  <dd>
                    {data.runs.length}
                    <span>original animations</span>
                  </dd>
                </div>
              </dl>
            </section>
            <section className="comparison-workspace" id="gallery" aria-label="Model comparison">
              <div className="workspace-toolbar">
                <div className="view-switch" role="group" aria-label="Table view">
                  <button
                    type="button"
                    data-view="artwork"
                    aria-pressed={state.view === 'artwork'}
                    onClick={() => update({ view: 'artwork' })}
                  >
                    <Grid2X2 size={15} aria-hidden="true" />
                    Artwork
                  </button>
                  <button
                    type="button"
                    data-view="runs"
                    aria-pressed={state.view === 'runs'}
                    onClick={() => update({ view: 'runs' })}
                  >
                    <Table2 size={15} aria-hidden="true" />
                    Run data
                  </button>
                </div>
                <div className="table-actions">
                  <button
                    type="button"
                    className="button mobile-filters"
                    aria-label="Filters"
                    onClick={() => setFilterOpen(true)}
                  >
                    <SlidersHorizontal size={15} aria-hidden="true" />
                    Filters{chips.length > 0 && <span className="count-badge">{chips.length}</span>}
                  </button>
                  <div className="search-field">
                    <Search size={15} aria-hidden="true" />
                    <input
                      ref={searchInput}
                      type="search"
                      id="brief-search"
                      aria-label="Search briefs"
                      placeholder="Search briefs…"
                      value={state.query}
                      onChange={(event) => update({ query: event.target.value }, 'replace')}
                    />
                    <kbd>{navigator.platform.includes('Mac') ? '⌘ K' : 'Ctrl K'}</kbd>
                  </div>
                  <button
                    type="button"
                    className="button share-button"
                    id="share-link"
                    aria-label={copied ? 'Link copied' : 'Share view'}
                    onClick={shareView}
                  >
                    {copied ? (
                      <Check size={15} aria-hidden="true" />
                    ) : (
                      <Link size={15} aria-hidden="true" />
                    )}
                    <span>{copied ? 'Link copied' : 'Share view'}</span>
                  </button>
                  <button
                    type="button"
                    className="button button-primary"
                    id="export-csv"
                    disabled={!runs.length}
                    onClick={exportCSV}
                  >
                    <Download size={15} aria-hidden="true" />
                    <span>Export CSV</span>
                  </button>
                </div>
              </div>
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
              {chips.length > 0 && (
                <div className="active-filters" aria-label="Active filters">
                  {chips.map((chip) => (
                    <button
                      type="button"
                      className="filter-chip"
                      key={chip.label}
                      aria-label={`Clear filter: ${chip.label}`}
                      onClick={chip.clear}
                    >
                      {chip.label}
                      <X size={11} aria-hidden="true" />
                    </button>
                  ))}
                  <button type="button" className="text-button" onClick={reset}>
                    Clear all
                  </button>
                </div>
              )}
              <div className="table-controls">
                <p className="result-count" id="result-count" role="status">
                  <strong>{runs.length}</strong>{' '}
                  {state.view === 'artwork'
                    ? runs.length === 1
                      ? 'artwork'
                      : 'artworks'
                    : runs.length === 1
                      ? 'run'
                      : 'runs'}
                  <span>·</span>
                  {selectedTasks.length} {selectedTasks.length === 1 ? 'brief' : 'briefs'}
                  <span>·</span>
                  {selectedModels.length} {selectedModels.length === 1 ? 'model' : 'models'}
                </p>
                <div className="display-controls">
                  <div className="comparison-presets" role="group" aria-label="Model presets">
                    <button
                      type="button"
                      aria-pressed={state.models.length === models.length}
                      onClick={allModels}
                    >
                      All models
                    </button>
                    <button
                      type="button"
                      title="The last recorded model for each selected provider"
                      onClick={latest}
                    >
                      Latest per provider
                    </button>
                  </div>
                  {state.view === 'artwork' && (
                    <>
                      <span className="control-divider" />
                      <label className="zoom-control">
                        Scale
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
                        className="background-controls"
                        role="group"
                        aria-label="Artwork background"
                      >
                        {(['grid', 'light', 'dark'] as Background[]).map((background) => (
                          <button
                            type="button"
                            key={background}
                            data-background={background}
                            aria-label={`${title(background)} background`}
                            title={`${title(background)} background`}
                            aria-pressed={state.background === background}
                            onClick={() => update({ background })}
                          >
                            <span className={`background-swatch ${background}`} />
                          </button>
                        ))}
                      </div>
                      <label className="stats-toggle">
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
                </div>
              </div>
              <div
                className={`table-scroll background-${state.background}`}
                ref={surface}
                onScroll={measure}
                tabIndex={runs.length ? 0 : -1}
                aria-label="Scrollable comparison table"
                style={{ '--stage-height': `${48 * state.zoom + 24}px` } as CSSProperties}
              >
                {runs.length ? (
                  state.view === 'artwork' ? (
                    <ArtworkTable
                      models={selectedModels}
                      tasks={selectedTasks}
                      zoom={state.zoom}
                      stats={state.stats}
                      width={columnWidth}
                      briefWidth={briefWidth}
                      inspect={setInspected}
                      openBrief={setBrief}
                      hideModel={(id) =>
                        update({ models: state.models.filter((model) => model !== id) })
                      }
                    />
                  ) : (
                    <RunsTable
                      runs={runs}
                      models={models}
                      state={state}
                      sort={sort}
                      inspect={setInspected}
                    />
                  )
                ) : (
                  <div className="empty-state">
                    <div>
                      <Palette size={25} aria-hidden="true" />
                    </div>
                    <h2>No matching results</h2>
                    <p>Try a different brief, or include more providers and models.</p>
                    <button type="button" className="button button-primary" onClick={reset}>
                      Reset filters
                    </button>
                  </div>
                )}
              </div>
              <div className="table-footer">
                <span>
                  <Info size={13} aria-hidden="true" />
                  {state.view === 'runs' || state.stats
                    ? '— means not reported.'
                    : 'Click any artwork to inspect its original run.'}
                </span>
                {scroll.previous || scroll.next ? (
                  <div className="table-navigation">
                    <span>More models</span>
                    <button
                      type="button"
                      className="icon-button"
                      aria-label="Scroll to previous columns"
                      disabled={!scroll.previous}
                      onClick={() => surface.current?.scrollBy({ left: -columnWidth * 2 })}
                    >
                      <ChevronLeft size={16} aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      className="icon-button"
                      aria-label="Scroll to next columns"
                      disabled={!scroll.next}
                      onClick={() => surface.current?.scrollBy({ left: columnWidth * 2 })}
                    >
                      <ChevronRight size={16} aria-hidden="true" />
                    </button>
                  </div>
                ) : (
                  <span className="loop-note">10 frames · 1-second loops</span>
                )}
              </div>
            </section>
            <div className="method-note">
              <span>
                <Info size={13} aria-hidden="true" />
                Counts use different client reporting methods; they are not an efficiency ranking.
              </span>
              <button type="button" onClick={() => setMethodOpen(true)}>
                Read the methodology
                <ArrowUpRight size={12} aria-hidden="true" />
              </button>
            </div>
            <footer className="page-footer">
              <span>Atelier · An experiment in art created through tools</span>
              <a href={`${repository}/tree/master/showcase`}>
                Source data &amp; replays
                <ArrowUpRight size={12} aria-hidden="true" />
              </a>
            </footer>
          </main>
        </div>
      </div>
      <Modal
        open={filterOpen}
        onOpenChange={setFilterOpen}
        title="Filters"
        description="Filter the comparison by provider, model, and brief."
        className="filter-modal"
      >
        <FilterPanel state={state} update={update} reset={reset} />
        <div className="filter-modal-footer">
          <button
            type="button"
            className="button button-primary"
            onClick={() => setFilterOpen(false)}
          >
            Show {runs.length} results
          </button>
        </div>
      </Modal>
      <RunInspector run={inspected} close={() => setInspected(null)} />
      <Modal
        open={Boolean(brief)}
        onOpenChange={(open) => {
          if (!open) setBrief(null);
        }}
        title={brief ? `${title(brief)} brief` : 'Frozen brief'}
        label="The same prompt for every model"
        description="The exact frozen prompt used for this artwork."
      >
        {brief && (
          <div className="brief-modal-content">
            <pre>{briefs[brief]}</pre>
            <a className="button" href={asset(`tasks/${brief}.txt`)} download>
              <Download size={14} aria-hidden="true" />
              Download prompt
            </a>
          </div>
        )}
      </Modal>
      <Modal
        open={methodOpen}
        onOpenChange={setMethodOpen}
        title="The Atelier experiment"
        label="Behind every pixel"
        description="The original methodology, server provenance, and reproduction notes."
        className="method-modal"
      >
        <div className="method-intro">
          <p>
            Every model receives the same ten frozen briefs and creates its artwork through
            Atelier’s editing tools. The table shows their original animations.
          </p>
          <div>
            <span>
              <Check size={14} aria-hidden="true" />
              Identical prompts
            </span>
            <span>
              <Check size={14} aria-hidden="true" />
              Original pixels
            </span>
            <span>
              <Check size={14} aria-hidden="true" />
              Replay journals
            </span>
          </div>
        </div>
        <div className="method-body">
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
        </div>
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
      </Modal>
    </>
  );
}
