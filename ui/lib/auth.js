// Control-plane credentials (RUSTYBIN_CONTROL_AUTH = token | jwt).
//
// A token arrives in the URL fragment (#token=...; fragments never reach the
// server) or through the sign-in screen. It is kept in memory and in
// sessionStorage (this tab only, gone when the tab closes), stripped from the
// URL at once, and sent as `Authorization: Bearer` on control-plane calls
// (see http.js). A JWT's `exp` claim schedules the "session expired" screen.

const KEY = 'rustybin.console.controlToken';
const listeners = new Set();

function meta(name) {
  const el = document.querySelector('meta[name="rustybin-' + name + '"]');
  return el ? (el.getAttribute('content') || '').trim() : '';
}

/** Control-plane auth mode announced by the server: open | token | jwt. */
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

function emit(kind) {
  for (const fn of listeners) {
    try { fn(kind); } catch (e) { console.error(e); }
  }
}

/** True when the server protects its control plane. */
export function required() {
  return mode === 'token' || mode === 'jwt';
}

export function getToken() {
  return token;
}

export function getReason() {
  return reason;
}

/** The token's JWT claims (not verified: display and expiry only), or null. */
export function claims() {
  const parts = token.split('.');
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

/** Expiry of a JWT token in ms since the epoch, or null. */
export function expiresAt() {
  const c = claims();
  return c && typeof c.exp === 'number' ? c.exp * 1000 : null;
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

/** Use a token (empty: forget it). */
export function setToken(t) {
  token = String(t || '').trim();
  try {
    if (token) sessionStorage.setItem(KEY, token);
    else sessionStorage.removeItem(KEY);
  } catch { /* storage may be unavailable: memory only */ }
  reason = token || !required() ? '' : 'missing';
  scheduleExpiry();
  emit(needsSignIn() ? 'required' : 'token');
}

export function signOut() {
  setToken('');
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
  if (found) setToken(found);
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
