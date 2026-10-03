// Console settings persisted per browser (theme, session, admin token,
// gateway URL), plus instance facts loaded at start (public mode, ...).

const KEY = 'rustybin.console.v1';
const listeners = new Set();

const defaults = {
  theme: 'system',
  session: '',
  adminToken: '',
  gatewayUrl: '',
  trafficSessionOnly: false,
};

function load() {
  try {
    const raw = localStorage.getItem(KEY);
    return Object.assign({}, defaults, raw ? JSON.parse(raw) : {});
  } catch {
    return Object.assign({}, defaults);
  }
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
  try { localStorage.setItem(KEY, JSON.stringify(state)); } catch { /* storage may be unavailable */ }
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
