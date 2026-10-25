// Console settings persisted per browser (theme, session, gateway URL),
// secrets per tab (the admin token, like the control token in auth.js), plus
// instance facts loaded at start (public mode, ...).

const KEY = 'rustybin.console.v1';
const SECRET_KEY = 'rustybin.console.v1.secrets';
/** Settings kept in this tab only (sessionStorage), never on disk. */
const SECRETS = ['adminToken'];
const listeners = new Set();

const defaults = {
  theme: 'system',
  session: '',
  adminToken: '',
  gatewayUrl: '',
  trafficSessionOnly: false,
};

function read(storage, key) {
  try {
    const raw = storage.getItem(key);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function save(all) {
  const kept = {};
  const secret = {};
  for (const [k, v] of Object.entries(all)) (SECRETS.includes(k) ? secret : kept)[k] = v;
  try { localStorage.setItem(KEY, JSON.stringify(kept)); } catch { /* storage may be unavailable */ }
  try { sessionStorage.setItem(SECRET_KEY, JSON.stringify(secret)); } catch { /* memory only */ }
}

function load() {
  const kept = read(localStorage, KEY);
  const secret = read(sessionStorage, SECRET_KEY);
  // Older consoles kept the admin token in localStorage: move it to this tab.
  const moved = SECRETS.filter((k) => k in kept);
  for (const k of moved) {
    if (!(k in secret)) secret[k] = kept[k];
    delete kept[k];
  }
  const all = Object.assign({}, defaults, kept, secret);
  if (moved.length) save(all);
  return all;
}

const state = load();

/** Instance facts (filled by app.js from /_rustybin/status and /config). */
export const instance = { publicMode: false, adminTokenConfigured: false, config: null, status: null };

export function get(key) {
  return state[key];
}

export function set(key, value) {
  if (state[key] === value) return;
  state[key] = value;
  save(state);
  for (const fn of listeners) {
    try { fn(key, value); } catch (e) { console.error(e); }
  }
}

/** Subscribe to changes; returns an unsubscribe function. */
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Per-tab scratch values that survive view switches (not persisted). */
const memory = new Map();
export function remember(key, value) { memory.set(key, value); }
export function recall(key, fallback) { return memory.has(key) ? memory.get(key) : fallback; }
