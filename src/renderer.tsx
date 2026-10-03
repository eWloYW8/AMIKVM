import { createContext, createElement, useCallback, useContext, useEffect, useRef, useState, type CSSProperties, type FormEvent, type MouseEvent } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { Activity, ArrowUpRight, Camera, ChevronRight, Circle, Clock3, FolderOpen, FolderSync, HardDrive, Info, Keyboard, KeyRound, LayoutGrid, LockKeyhole, Maximize, Monitor, MoreHorizontal, Pause, Play, Plug, Plus, Power, RefreshCw, Search, Server, ShieldCheck, Square, Star, TerminalSquare, Unplug, Users, Video as VideoIcon, X } from 'lucide-react';

export type Intent = Record<string, unknown>;
type Props = Record<string, unknown>;
export interface UiNode { kind: string; props: Props; children?: UiNode[] }
export interface UiModel { root: UiNode }
type Dispatch = (intent: Intent) => Promise<void>;
const Actions = createContext<Dispatch>(async () => {});
type InputMessage = { id: string; event: Record<string, unknown> };
const InputEvents = createContext<(message: InputMessage) => void>(() => {});
const Form = createContext<{ values: Props; set: (name: string, value: unknown) => void; busy: boolean } | null>(null);
const icons = { Activity, ArrowUpRight, Camera, ChevronRight, Circle, Clock3, FolderOpen, FolderSync, HardDrive, Info, Keyboard, KeyRound, LayoutGrid, LockKeyhole, Maximize, Monitor, MoreHorizontal, Pause, Play, Plug, Plus, Power, RefreshCw, Search, Server, ShieldCheck, Square, Star, TerminalSquare, Unplug, Users, Video: VideoIcon, X };
const tags = new Set(['div', 'span', 'aside', 'main', 'nav', 'header', 'footer', 'section', 'article', 'p', 'h1', 'h2', 'h3', 'strong', 'small', 'em']);

function Icon({ name, size = 17, fill }: { name: string; size?: number; fill?: string }) {
  const Component = icons[name as keyof typeof icons];
  return Component ? <Component size={size} fill={fill ?? 'none'} aria-hidden /> : null;
}
function Children({ nodes }: { nodes?: UiNode[] }) { return nodes?.map((node, i) => <Render key={String(node.props.key ?? i)} node={node} />); }

function Button({ node: { props: p, children } }: { node: UiNode }) {
  const dispatch = useContext(Actions);
  const sendInput = useContext(InputEvents);
  const form = useContext(Form);
  const active = useRef(false);
  const disabled = !!p.disabled || !!form?.busy;
  const up = useRef(p.inputOnUp as InputMessage | undefined);
  up.current = p.inputOnUp as InputMessage | undefined;
  function press() { if (!active.current && p.inputOnDown) { active.current = true; sendInput(p.inputOnDown as InputMessage); } }
  function release() { if (active.current) { active.current = false; if (up.current) sendInput(up.current); } }
  useEffect(() => () => { if (active.current && up.current) sendInput(up.current); }, [sendInput]);
  useEffect(() => { if (disabled && active.current) { active.current = false; if (up.current) sendInput(up.current); } }, [disabled, sendInput]);
  return <button type={p.type === 'submit' ? 'submit' : 'button'} className={String(p.className ?? '')} title={p.title as string | undefined} aria-label={(p.title ?? p.text) as string} aria-pressed={p.pressed as boolean | undefined} disabled={disabled}
    onPointerDown={e => { if (p.inputOnDown) { e.preventDefault(); e.currentTarget.setPointerCapture(e.pointerId); press(); } }} onMouseDown={e => { if (p.preserveFocus) e.preventDefault(); }} onPointerUp={release} onPointerCancel={release} onLostPointerCapture={release} onBlur={release}
    onKeyDown={e => { if (p.inputOnDown && (e.key === ' ' || e.key === 'Enter')) { e.preventDefault(); if (!e.repeat) press(); } }} onKeyUp={e => { if (p.inputOnDown && (e.key === ' ' || e.key === 'Enter')) { e.preventDefault(); release(); } }}
    onClick={() => { if (p.action) void dispatch(p.confirm ? { action: 'confirm_action', message: String(p.confirm), intent: p.action } : p.action as Intent); }}>
    {p.icon ? <Icon name={String(p.icon)} fill={p.fill as string | undefined} /> : null}{p.text ? String(p.text) : null}<Children nodes={children} />
  </button>;
}

function FormNode({ node: { props: p, children } }: { node: UiNode }) {
  const dispatch = useContext(Actions);
  const [values, setValues] = useState<Props>(p.values as Props);
  const [busy, setBusy] = useState(false);
  async function submit(event: FormEvent) { event.preventDefault(); if (busy) return; setBusy(true); try { await dispatch({ ...(p.action as Intent), values }); } finally { setBusy(false); } }
  return <Form.Provider value={{ values, set: (name, value) => setValues(previous => ({ ...previous, [name]: value })), busy }}><form onSubmit={e => void submit(e)}><Children nodes={children} /></form></Form.Provider>;
}

function Input({ props: p, field = false }: { props: Props; field?: boolean }) {
  const dispatch = useContext(Actions);
  const form = useContext(Form);
  const [value, setValue] = useState<unknown>(p.value ?? '');
  const editing = useRef(false);
  useEffect(() => { if (!editing.current || p.type === 'select' || p.type === 'checkbox') setValue(p.value ?? ''); }, [p]);
  const name = String(p.name ?? '');
  const current = form ? form.values[name] : value;
  function change(next: unknown) { if (form) form.set(name, next); else { setValue(next); if (p.action) void dispatch({ ...(p.action as Intent), value: next }); } }
  const shared = { name, disabled: !!p.disabled || !!form?.busy, required: !!p.required, autoFocus: !!p.autoFocus, 'data-autofocus': !!p.autoFocus, 'aria-label': String(p.label ?? ''), onFocus: () => { editing.current = true; }, onBlur: () => { editing.current = false; } };
  let control;
  if (p.type === 'select') control = <select {...shared} value={String(current ?? '')} onChange={e => change(e.target.value)}>{(p.options as { value: string; label: string }[]).map(option => <option key={option.value} value={option.value}>{option.label}</option>)}</select>;
  else if (p.type === 'textarea') control = <textarea {...shared} rows={Number(p.rows ?? 3)} value={String(current ?? '')} placeholder={p.placeholder as string | undefined} onChange={e => change(e.target.value)} />;
  else control = <input {...shared} type={String(p.type ?? 'text')} value={p.type === 'checkbox' ? undefined : String(current ?? '')} checked={p.type === 'checkbox' ? !!current : undefined} min={p.min as number | undefined} max={p.max as number | undefined} step={p.step as number | undefined} maxLength={p.maxLength as number | undefined} autoComplete={p.autoComplete as string | undefined} placeholder={p.placeholder as string | undefined} onChange={e => change(p.type === 'checkbox' ? e.target.checked : e.target.value)} />;
  return field ? <label className={p.className as string}>{p.type === 'checkbox' ? <>{control}{String(p.label)}</> : <>{String(p.label)}{control}</>}</label> : control;
}

function DialogNode({ node: { props: p, children } }: { node: UiNode }) {
  const dispatch = useContext(Actions);
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => { const dialog = ref.current; if (dialog && !dialog.open) { dialog.showModal(); dialog.querySelector<HTMLElement>('[data-autofocus="true"]')?.focus(); } }, []);
  return <dialog ref={ref} className={p.className as string} onCancel={e => { e.preventDefault(); void dispatch(p.action as Intent); }} onClick={e => { if (e.target === ref.current) void dispatch(p.action as Intent); }}><Children nodes={children} /></dialog>;
}

function Video({ props: p }: { props: Props }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const sendInput = useContext(InputEvents);
  const error = useContext(Actions);
  const current = useRef(p);
  current.current = p;
  const captureRequest = useRef<string | null>(null);
  useEffect(() => {
    const element = canvas.current;
    if (!element || !p.serverId) return;
    let pending = 0;
    const update = () => {
      pending = 0;
      const bounds = element.getBoundingClientRect();
      const clip = element.parentElement?.getBoundingClientRect();
      const rect = (r: DOMRect) => ({ left: r.left, top: r.top, width: r.width, height: r.height });
      sendInput({ id: String(p.serverId), event: { type: 'viewport', viewport: p.enabled && p.visible && bounds.width > 0 && bounds.height > 0 && clip ? { bounds: rect(bounds), clip: rect(clip), scale: window.devicePixelRatio } : null } });
    };
    const schedule = () => { if (!pending) pending = requestAnimationFrame(update); };
    const observer = new ResizeObserver(schedule);
    observer.observe(element);
    if (element.parentElement) observer.observe(element.parentElement);
    window.addEventListener('resize', schedule);
    window.addEventListener('scroll', schedule, true);
    schedule();
    return () => {
      cancelAnimationFrame(pending); observer.disconnect();
      window.removeEventListener('resize', schedule);
      window.removeEventListener('scroll', schedule, true);
      sendInput({ id: String(p.serverId), event: { type: 'viewport', viewport: null } });
    };
  }, [p.serverId, p.enabled, p.visible, sendInput]);
  useEffect(() => { if (p.cursorFocus) canvas.current?.focus({ preventScroll: true }); }, [p.cursorFocus]);
  useEffect(() => {
    const focus = () => sendInput({ id: String(p.serverId), event: { type: 'focus', focused: !document.hidden && document.activeElement === canvas.current } });
    window.addEventListener('focus', focus);
    window.addEventListener('blur', focus);
    document.addEventListener('visibilitychange', focus);
    focus();
    return () => {
      window.removeEventListener('focus', focus);
      window.removeEventListener('blur', focus);
      document.removeEventListener('visibilitychange', focus);
      sendInput({ id: String(p.serverId), event: { type: 'focus', focused: false } });
    };
  }, [p.serverId, sendInput]);
  useEffect(() => {
    function result(locked: boolean, failed = false) {
      const token = captureRequest.current;
      if (token) sendInput({ id: String(p.serverId), event: { type: 'pointer_capture', token, locked, failed } });
      if (!locked) captureRequest.current = null;
    }
    function changed() {
      const locked = document.pointerLockElement === canvas.current;
      if (locked && (captureRequest.current !== current.current.captureToken || !current.current.enabled || !current.current.visible)) document.exitPointerLock();
      else result(locked);
    }
    const failed = () => result(false, true);
    document.addEventListener('pointerlockchange', changed);
    document.addEventListener('pointerlockerror', failed);
    return () => {
      document.removeEventListener('pointerlockchange', changed);
      document.removeEventListener('pointerlockerror', failed);
      if (document.pointerLockElement === canvas.current) document.exitPointerLock();
      result(false);
    };
  }, [p.serverId, sendInput]);
  useEffect(() => {
    if (document.pointerLockElement === canvas.current && (!p.enabled || !p.visible || captureRequest.current !== p.captureToken)) document.exitPointerLock();
  }, [p.enabled, p.visible, p.captureToken]);
  useEffect(() => {
    if (!p.streaming) return;
    let alive = true;
    let latest: ArrayBuffer | null = null;
    let pending = 0;
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = frame => {
      if (!alive) return;
      latest = frame;
      if (!pending) pending = requestAnimationFrame(() => {
        pending = 0;
        const frame = latest;
        const target = canvas.current;
        if (!alive || !frame || !target || frame.byteLength < 12) return;
        const header = new DataView(frame);
        const width = header.getUint32(0, true), height = header.getUint32(4, true);
        if (!width || !height || frame.byteLength !== 12 + width * height * 4) return;
        if (target.width !== width || target.height !== height) { target.width = width; target.height = height; }
        target.getContext('2d')?.putImageData(new ImageData(new Uint8ClampedArray(frame, 12), width, height), 0, 0);
      });
    };
    const subscription = invoke<number>('subscribe_video', { id: p.serverId, onFrame: channel });
    void subscription.catch(message => { if (alive) void error({ action: 'interaction_error', message: String(message) }); });
    return () => { alive = false; cancelAnimationFrame(pending); void subscription.then(channelId => invoke('unsubscribe_video', { id: p.serverId, channelId })).catch(() => {}); };
  }, [p.serverId, p.streaming, error]);
  function send(event: Record<string, unknown>) {
    if (!p.enabled) return;
    sendInput({ id: String(p.serverId), event });
  }
  function pointer(e: MouseEvent<HTMLCanvasElement>, entered = false) {
    if (!p.enabled) return;
    const bounds = e.currentTarget.getBoundingClientRect();
    send({ type: 'pointer', buttons: e.buttons, x: e.clientX - bounds.left, y: e.clientY - bounds.top, width: Math.round(bounds.width), height: Math.round(bounds.height), dx: e.movementX, dy: e.movementY, wheel: 0, entered, capture: document.pointerLockElement === e.currentTarget ? captureRequest.current : null });
  }
  function down(e: MouseEvent<HTMLCanvasElement>) {
    e.preventDefault(); e.currentTarget.focus();
    if (p.captureToken && document.pointerLockElement !== e.currentTarget && !captureRequest.current) {
      const token = String(p.captureToken);
      captureRequest.current = token;
      try {
        const request = e.currentTarget.requestPointerLock();
        void request?.catch(() => { if (captureRequest.current === token) { captureRequest.current = null; sendInput({ id: String(p.serverId), event: { type: 'pointer_capture', token, locked: false, failed: true } }); } });
      } catch {
        captureRequest.current = null;
        sendInput({ id: String(p.serverId), event: { type: 'pointer_capture', token, locked: false, failed: true } });
      }
    }
    pointer(e);
  }
  return <canvas ref={canvas} style={{ ...(p.style as CSSProperties), visibility: p.visible ? 'visible' : 'hidden' }} tabIndex={p.enabled || p.keyboardEvents ? 0 : -1} data-server={String(p.serverId)} onContextMenu={e => e.preventDefault()} onKeyDown={e => { if (p.keyboardEvents) { e.preventDefault(); if (!e.repeat) sendInput({ id: String(p.serverId), event: { type: 'key', code: e.code, key: e.key, location: e.location, pressed: true, modifiers: { ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey, altGraph: e.getModifierState('AltGraph'), capsLock: e.getModifierState('CapsLock') } } }); } }} onKeyUp={e => { e.preventDefault(); sendInput({ id: String(p.serverId), event: { type: 'key', code: e.code, key: e.key, location: e.location, pressed: false, modifiers: { ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey, altGraph: e.getModifierState('AltGraph'), capsLock: e.getModifierState('CapsLock') } } }); }} onFocus={() => sendInput({ id: String(p.serverId), event: { type: 'focus', focused: true } })} onBlur={() => sendInput({ id: String(p.serverId), event: { type: 'focus', focused: false } })} onPointerDown={e => { e.currentTarget.focus(); e.currentTarget.setPointerCapture(e.pointerId); }} onMouseDown={down} onMouseUp={e => pointer(e)} onMouseMove={e => pointer(e)} onMouseEnter={e => pointer(e, true)} onWheel={e => { e.preventDefault(); const bounds = e.currentTarget.getBoundingClientRect(); send({ type: 'pointer', buttons: e.buttons, x: e.clientX - bounds.left, y: e.clientY - bounds.top, width: Math.round(bounds.width), height: Math.round(bounds.height), dx: 0, dy: 0, wheel: e.deltaY, capture: document.pointerLockElement === e.currentTarget ? captureRequest.current : null }); }} />;
}

function Render({ node }: { node: UiNode }) {
  const p = node.props;
  switch (node.kind) {
    case 'element': return createElement(tags.has(String(p.tag)) ? String(p.tag) : 'div', { className: p.className, lang: p.lang }, <Children nodes={node.children} />);
    case 'text': return createElement(tags.has(String(p.tag)) ? String(p.tag) : 'span', { className: p.className }, String(p.text));
    case 'icon': return <Icon name={String(p.name)} size={Number(p.size ?? 17)} />;
    case 'image': return <img src={String(p.src)} alt={String(p.alt)} />;
    case 'button': return <Button node={node} />;
    case 'input': return <Input props={p} />;
    case 'field': return <Input props={p} field />;
    case 'form': return <FormNode node={node} />;
    case 'dialog': return <DialogNode node={node} />;
    case 'video': return <Video props={p} />;
    default: return null;
  }
}

export default function Renderer({ model, dispatch }: { model: UiModel; dispatch: Dispatch }) {
  const queues = useRef(new Map<string, Promise<unknown>>());
  const sendInput = useCallback((message: InputMessage) => {
    const previous = queues.current.get(message.id) ?? Promise.resolve();
    const pending = previous.then(() => invoke('ui_input', message)).catch(error => { void dispatch({ action: 'interaction_error', message: String(error) }); });
    queues.current.set(message.id, pending);
    void pending.finally(() => { if (queues.current.get(message.id) === pending) queues.current.delete(message.id); });
  }, [dispatch]);
  const activeServers = useRef<string[]>([]);
  activeServers.current = (model.root.props.inputServers as string[] | undefined) ?? [];
  useEffect(() => {
    const release = () => { for (const id of activeServers.current) sendInput({ id, event: { type: 'release_all' } }); };
    const visibility = () => { if (document.hidden) release(); };
    window.addEventListener('blur', release); document.addEventListener('visibilitychange', visibility);
    return () => { window.removeEventListener('blur', release); document.removeEventListener('visibilitychange', visibility); release(); };
  }, [sendInput]);
  return <Actions.Provider value={dispatch}><InputEvents.Provider value={sendInput}><Render node={model.root} /></InputEvents.Provider></Actions.Provider>;
}
