// Control-plane credentials (RUSTYBIN_CONTROL_AUTH = password | token | jwt).
//
// A token arrives from the sign-in screen (a user name and password in
// password mode, the default: POST /_rustybin/login answers a control token;
// or a pasted token), or in the URL fragment (#token=...; fragments never
// reach the server). It is kept in memory and in sessionStorage (this tab
// only, gone when the tab closes), stripped from the URL at once, and sent as
// `Authorization: Bearer` on control-plane calls (see http.js). "Remember me"
// keeps a sign-in token in localStorage until it expires (12 hours at most).
// A JWT's `exp` claim schedules the "session expired" screen.

const KEY = 'rustybin.console.controlToken';
const REMEMBER_KEY = 'rustybin.console.rememberedToken';
const listeners = new Set();

function meta(name) {
  const el = document.querySelector('meta[name="rustybin-' + name + '"]');
  return el ? (el.getAttribute('content') || '').trim() : '';
}

/** Control-plane auth mode announced by the server: password | open | token | jwt. */
export const mode = meta('control-auth') || 'open';
/** RUSTYBIN_CONSOLE_TITLE, or ''. */
export const consoleTitle = meta('console-title');
/** RUSTYBIN_CONSOLE_BACKLINK when it is an http(s) URL, or ''. */
export const backlink = /^https?:\/\/[^\s"'<>`\\]+$/i.test(meta('console-backlink')) ? meta('console-backlink') : '';

let token = '';
// Why the sign-in screen is shown ('' = not needed): missing | expired |
// signed-out | the server's reason (invalid_signature, invalid_audience, ...).
let reason = '';
let expiryTimer = null;

try { token = sessionStorage.getItem(KEY) || ''; } catch { token = ''; }
// A remembered sign-in (password mode) outlives the tab.
if (!token && mode === 'password') {
  try {
    const kept = localStorage.getItem(REMEMBER_KEY) || '';
    if (kept && !tokenExpired(kept)) {
      token = kept;
      try { sessionStorage.setItem(KEY, token); } catch { /* memory only */ }
    } else if (kept) {
      localStorage.removeItem(REMEMBER_KEY);
    }
  } catch { /* storage may be unavailable */ }
}

function parseClaims(t) {
  const parts = String(t || '').split('.');
  if (parts.length !== 3) return null;
  try {
    const b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const json = atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4));
    const c = JSON.parse(json);
    return c && typeof c === 'object' ? c : null;
  } catch {
    return null;
  }
}

function tokenExpired(t) {
  const c = parseClaims(t);
  return !!(c && typeof c.exp === 'number' && c.exp * 1000 <= Date.now());
}

function emit(kind) {
  for (const fn of listeners) {
    try { fn(kind); } catch (e) { console.error(e); }
  }
}

/** True when the server protects its control plane. */
export function required() {
  return mode === 'password' || mode === 'token' || mode === 'jwt';
}

/** True when the sign-in screen asks for a user name and password. */
export function passwordMode() {
  return mode === 'password';
}

export function getToken() {
  return token;
}

export function getReason() {
  return reason;
}

/** The token's JWT claims (not verified: display and expiry only), or null. */
export function claims() {
  return parseClaims(token);
}

/** The signed-in user name of a sign-in token (claim sub "user:<name>"), or null. */
export function user() {
  const c = claims();
  const sub = c && typeof c.sub === 'string' ? c.sub : '';
  return sub.startsWith('user:') ? sub.slice(5) : null;
}

/** True when the token came from the password sign-in (claim rb_use "console"). */
export function signedInWithPassword() {
  const c = claims();
  return !!(c && c.rb_use === 'console');
}

/** Expiry of a JWT token in ms since the epoch, or null. */
export function expiresAt() {
  const c = claims();
  return c && typeof c.exp === 'number' ? c.exp * 1000 : null;
}

/**
 * The API key a shared instance's token is bound to (claim `rb_key`: the
 * platform's "Open console" for a key), or null. Such a console sees that
 * key's data only, and sends the token as X-Rustybin-Key-Token on its own
 * data-plane requests (it never holds the key).
 */
export function boundKey() {
  const c = claims();
  return c && typeof c.rb_key === 'string' && c.rb_key ? c.rb_key : null;
}

/** Scopes of a JWT token (absent claim: console), or null for an opaque token. */
export function scopes() {
  const c = claims();
  if (!c) return null;
  if (c.scope === undefined || c.scope === null) return ['console'];
  if (Array.isArray(c.scope)) return c.scope.map(String);
  return String(c.scope).split(/\s+/).filter(Boolean);
}

/** True when the console must show the sign-in screen. */
export function needsSignIn() {
  if (!required()) return false;
  return !token || reason !== '';
}

function scheduleExpiry() {
  clearTimeout(expiryTimer);
  const at = expiresAt();
  if (!at) return;
  const ms = at - Date.now();
  if (ms <= 0) {
    reason = 'expired';
    return;
  }
  expiryTimer = setTimeout(() => markUnauthorized('expired'), Math.min(ms, 0x7fffffff));
}

/** Use a token (empty: forget it). `remember` keeps it across tabs and restarts of the browser. */
export function setToken(t, remember) {
  token = String(t || '').trim();
  try {
    if (token) sessionStorage.setItem(KEY, token);
    else sessionStorage.removeItem(KEY);
  } catch { /* storage may be unavailable: memory only */ }
  try {
    if (token && remember) localStorage.setItem(REMEMBER_KEY, token);
    else if (!token || remember === false) localStorage.removeItem(REMEMBER_KEY);
  } catch { /* storage may be unavailable */ }
  reason = token || !required() ? '' : 'missing';
  scheduleExpiry();
  emit(needsSignIn() ? 'required' : 'token');
}

/**
 * Sign in with a user name and password (password mode): POST
 * /_rustybin/login on `server`. Resolves {ok, status, reason, retryAfter,
 * message}; on success the token is in use.
 */
export async function signIn(server, username, password, remember) {
  let resp;
  try {
    resp = await fetch(server + '/_rustybin/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ username, password }),
      credentials: 'omit',
      cache: 'no-store',
    });
  } catch {
    return { ok: false, status: 0, reason: 'unreachable', message: 'Rustybin could not be reached: try again.' };
  }
  let json = null;
  try { json = await resp.json(); } catch { json = null; }
  if (resp.ok && json && json.token) {
    setToken(json.token, !!remember);
    return { ok: true, status: resp.status, json };
  }
  const reason = (json && json.reason) || (resp.status === 429 ? 'rate_limited' : 'invalid_token');
  const retryAfter = Number(resp.headers.get('Retry-After') || (json && json.retry_after) || 0) || 0;
  return { ok: false, status: resp.status, reason, retryAfter, message: (json && json.error) || ('Sign-in failed (' + resp.status + ').') };
}

/**
 * Sign out: a sign-in token is revoked on the server (POST /_rustybin/logout,
 * best effort), then every copy is forgotten.
 */
export function signOut(server) {
  const t = token;
  if (t && mode === 'password' && server) {
    try {
      fetch(server + '/_rustybin/logout', {
        method: 'POST',
        headers: { Authorization: 'Bearer ' + t, Accept: 'application/json' },
        credentials: 'omit',
        keepalive: true,
      }).catch(() => {});
    } catch { /* ignore */ }
  }
  setToken('', false);
  reason = 'signed-out';
  emit('required');
}

/** A control-plane call was refused with 401 (`why`: the server's reason). */
export function markUnauthorized(why) {
  if (!required()) return;
  const next = !token ? 'missing' : (why || 'invalid_token');
  if (reason === next) return;
  reason = next;
  emit('required');
}

/**
 * Move `#token=...` (also `#access_token=...`, or `&token=...` after a view
 * route such as `#/traffic&token=...`) into memory and strip it from the URL.
 */
export function consumeFragment() {
  const raw = (location.hash || '').replace(/^#/, '');
  if (!/(^|&)(access_)?token=/.test(raw)) return false;
  const rest = [];
  let found = '';
  for (const part of raw.split('&')) {
    const m = /^(?:access_)?token=(.*)$/.exec(part);
    if (m) {
      try { found = decodeURIComponent(m[1]); } catch { found = m[1]; }
    } else if (part) {
      rest.push(part);
    }
  }
  const hash = rest.length ? '#' + rest.join('&') : '';
  try { history.replaceState(history.state, '', location.pathname + location.search + hash); } catch { location.hash = hash; }
  if (!found) return false; // an empty token=: just route the rest
  setToken(found);
  return true;
}

/** Subscribe to 'token' (signed in) and 'required' (sign-in needed) events. */
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

// A stored JWT may have expired while the tab was closed.
if (token) scheduleExpiry();
if (required() && !token) reason = 'missing';
