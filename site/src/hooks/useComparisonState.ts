import { useCallback, useEffect, useRef, useState } from 'react';
import { data, models } from '../data.ts';
import { readState, stateURL } from '../lib/comparison.ts';
import type { ComparisonState } from '../lib/comparison.ts';

type Update = Partial<ComparisonState> | ((current: ComparisonState) => Partial<ComparisonState>);

export function useComparisonState() {
  const [state, setState] = useState(() => readState(window.location.search, models, data.tasks));
  const current = useRef(state);
  const update = useCallback((patch: Update, history: 'push' | 'replace' = 'push') => {
    const next = {
      ...current.current,
      ...(typeof patch === 'function' ? patch(current.current) : patch),
    };
    current.current = next;
    const url = stateURL(next, models, window.location.href);
    if (url.href !== window.location.href) {
      window.history[history === 'push' ? 'pushState' : 'replaceState'](null, '', url);
    }
    setState(next);
  }, []);
  useEffect(() => {
    const restore = () => {
      const next = readState(window.location.search, models, data.tasks);
      current.current = next;
      setState(next);
    };
    window.addEventListener('popstate', restore);
    return () => window.removeEventListener('popstate', restore);
  }, []);
  return { state, update };
}
