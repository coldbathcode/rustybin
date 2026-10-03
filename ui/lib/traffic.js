// Live traffic store: backfills from /_rustybin/requests and follows
// /_rustybin/requests/stream (SSE). Runs for the whole console session so no
// request is missed while the presenter is on another view.
import * as settings from './settings.js';
import { SERVER, getJson } from './http.js';

const MAX = 1000;
const entries = []; // newest first
const listeners = new Set();
let source = null;
let status = 'idle'; // idle | connecting | live | error | needs-session
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
  if (settings.instance.publicMode && !settings.get('session')) {
    setStatus('needs-session', 'This instance runs in public mode: set a session to see your requests.');
    return;
  }
  setStatus('connecting');
  backfill().catch((e) => setStatus('error', e.message));
  const q = query();
  try {
    source = new EventSource(SERVER + '/_rustybin/requests/stream' + (q ? '?' + q : ''));
  } catch (e) {
    setStatus('error', String(e));
    return;
  }
  source.addEventListener('open', () => setStatus('live'));
  source.addEventListener('request', (ev) => {
    try { add(JSON.parse(ev.data), true); } catch { /* ignore malformed */ }
  });
  source.addEventListener('error', () => {
    // EventSource retries by itself; report the state meanwhile.
    if (source && source.readyState === EventSource.CLOSED) {
      setStatus('error', 'Live feed closed, retrying in 3 s');
      retryTimer = setTimeout(connect, 3000);
    } else {
      setStatus('connecting', 'Reconnecting to the live feed');
    }
  });
}

export function disconnect() {
  if (source) {
    source.close();
    source = null;
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
