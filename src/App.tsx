import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import Renderer, { type Intent, type UiModel } from './renderer';

export default function App() {
  const [model, setModel] = useState<UiModel | null>(null);
  const [error, setError] = useState('');
  const request = useRef(0);
  const refresh = useCallback(async () => {
    const number = ++request.current;
    try { const model = await invoke<UiModel>('ui_snapshot'); if (number === request.current) setModel(model); }
    catch (error) { setError(String(error)); }
  }, []);
  useEffect(() => {
    if (!isTauri()) { setError('Open this interface in the AMIKVM desktop application.'); return; }
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const subscriptions = ['ui-changed', 'session-state'].map(event => listen(event, () => { if (alive && !timer) timer = setTimeout(() => { timer = undefined; void refresh(); }, 30); }));
    void refresh();
    return () => { alive = false; clearTimeout(timer); for (const subscription of subscriptions) void subscription.then(unlisten => unlisten()); };
  }, [refresh]);
  const dispatch = useCallback(async (intent: Intent) => {
    const number = ++request.current;
    try { const model = await invoke<UiModel>('ui_action', { intent }); if (number === request.current) setModel(model); }
    catch (error) { setError(String(error)); }
  }, []);
  return <>{model ? <Renderer model={model} dispatch={dispatch} /> : <div className="startup">AMIKVM<span>{error}</span></div>}{model && error && <div className="toast" role="alert"><span>{error}</span><button type="button" onClick={() => setError('')}>×</button></div>}</>;
}
