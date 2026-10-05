// Live traffic store: backfills from /_rustybin/requests and follows
// /_rustybin/requests/stream (SSE). Runs for the whole console session so no
// request is missed while the presenter is on another view.
//
// The feed is read with fetch (not EventSource) so it can carry the
// control-plane token in an Authorization header; it reconnects (and
// backfills, deduplicated by id) after a drop.
import * as settings from './settings.js';
import * as auth from './auth.js';
import { SERVER, getJson, send, readText, sseParser } from './http.js';

const MAX = 1000;
const entries = []; // newest first
const listeners = new Set();
let feed = null; // AbortController of the running stream
let status = 'idle'; // idle | connecting | live | error | needs-session | signed-out
let lastError = '';
let unseen = 0;
let viewing = false;
let retryTimer = null;

function emit(kind, entry) {
  for (const fn of listeners) {
    try { fn(kind, entry); } catch (e) { console.error(e); }
  }
}

function query() {
  const p = new URLSearchParams();
  const session = settings.get('session');
  // Public mode only serves the caller's session; otherwise every request is
  // shown unless the presenter narrowed the feed to the console session.
  if (session && (settings.instance.publicMode || settings.get('trafficSessionOnly'))) p.set('session', session);
  return p.toString();
}

function add(entry, live) {
  if (entries.some((e) => e.id === entry.id)) return;
  entries.unshift(entry);
  if (entries.length > MAX) entries.length = MAX;
  if (live && !viewing) unseen++;
  emit('add', entry);
}

async function backfill() {
  const q = query();
  const data = await getJson('/_rustybin/requests?limit=200' + (q ? '&' + q : ''));
  const list = (data.requests || []).slice().reverse();
  for (const e of list) add(e, false);
}

function setStatus(s, err) {
  status = s;
  lastError = err || '';
  emit('status');
}

export function connect() {
  disconnect();
  clearTimeout(retryTimer);
  if (auth.needsSignIn()) {
    setStatus('signed-out', 'Sign in to see the live feed.');
    return;
  }
  if (settings.instance.publicMode && !settings.get('session')) {
    setStatus('needs-session', 'This instance runs in public mode: set a session to see your requests.');
    return;
  }
  setStatus('connecting');
  backfill().catch((e) => setStatus('error', e.message));
  const ctrl = new AbortController();
  feed = ctrl;
  follow(ctrl).catch((e) => {
    if (feed === ctrl) retry(String(e && e.message ? e.message : e));
  });
}

function retry(message) {
  setStatus('error', message + ' Retrying in 3 s.');
  clearTimeout(retryTimer);
  retryTimer = setTimeout(connect, 3000);
}

async function follow(ctrl) {
  const q = query();
  const r = await send({
    url: SERVER + '/_rustybin/requests/stream' + (q ? '?' + q : ''),
    headers: [['Accept', 'text/event-stream']],
    stream: true,
    timeout: 15000,
    signal: ctrl.signal,
  });
  if (feed !== ctrl) return;
  if (r.error) {
    retry(r.error);
    return;
  }
  if (!r.ok) {
    if (r.status === 401 || r.status === 403) {
      setStatus(r.status === 401 ? 'signed-out' : 'error', r.status === 401 ? 'Sign in to see the live feed.' : 'This token may not read the live feed.');
      return;
    }
    retry('Live feed answered ' + r.status + '.');
    return;
  }
  setStatus('live');
  const parse = sseParser((ev) => {
    if (ev.event !== 'request') return;
    try { add(JSON.parse(ev.data), true); } catch { /* ignore malformed */ }
  });
  try {
    await readText(r.response, parse, ctrl.signal);
  } catch { /* dropped */ }
  if (feed === ctrl && !ctrl.signal.aborted) {
    setStatus('connecting', 'Reconnecting to the live feed');
    clearTimeout(retryTimer);
    retryTimer = setTimeout(connect, 1000);
  }
}

export function disconnect() {
  if (feed) {
    const ctrl = feed;
    feed = null;
    ctrl.abort();
  }
}

export function all() { return entries; }
export function find(id) { return entries.find((e) => e.id === id); }
export function getStatus() { return { status, error: lastError }; }
export function unseenCount() { return unseen; }

export function setViewing(v) {
  viewing = v;
  if (v) { unseen = 0; emit('seen'); }
}

/** Remove entries locally (after a server-side clear). */
export function clearLocal() {
  entries.length = 0;
  unseen = 0;
  emit('clear');
}

export function subscribe(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

settings.onChange((key) => {
  if (key === 'session' || key === 'trafficSessionOnly') {
    clearLocal();
    connect();
  }
});

auth.onChange((kind) => {
  if (kind === 'required') {
    disconnect();
    clearTimeout(retryTimer);
    setStatus('signed-out', 'Sign in to see the live feed.');
  }
});
