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

import { randomString, pkceChallenge } from './crypto.js';

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
// Who the data this tab holds belongs to (see identityOf), '' before a sign-in.
let identity = '';
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

/**
 * Who a token signs in (its subject, key and issuer; an opaque token is its
 * own identity), or '' for no token. A new identity, or none, means the
 * data the console holds for the previous one must go.
 */
function identityOf(t) {
  if (!t) return '';
  const c = parseClaims(t);
  return c ? JSON.stringify([c.sub || '', c.rb_key || '', c.iss || '']) : 'opaque:' + t;
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

/**
 * The signed-in user name of a sign-in token (claim rb_user; a 0.3 token's
 * sub "user:<name>"), or null.
 */
export function user() {
  const c = claims();
  if (c && c.rb_use === 'console' && typeof c.rb_user === 'string') return c.rb_user;
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

/**
 * Use a token (empty: forget it). `remember` keeps it across tabs and
 * restarts of the browser. Forgetting the token, or a token for someone
 * else, first emits 'reset': the console drops what it holds of the
 * previous user (captured traffic, the admin token, the MCP inspector's
 * credentials, the assistant's history, the last export).
 */
export function setToken(t, remember) {
  token = String(t || '').trim();
  const next = identityOf(token);
  const changed = identity !== '' && next !== identity;
  if (token || changed) identity = next;
  if (changed) emit('reset');
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
 * message}; on success the token is in use. An account that opens only the
 * API portal (`portal_only`) is not signed in to the console: the answer
 * set the portal's cookie (Path=/portal, so the request keeps
 * same-origin credentials), its console token is revoked at once, and the
 * result says where to go (`portalUrl`).
 */
export async function signIn(server, username, password, remember) {
  let resp;
  try {
    resp = await fetch(server + '/_rustybin/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ username, password }),
      credentials: 'same-origin',
      cache: 'no-store',
    });
  } catch {
    return { ok: false, status: 0, reason: 'unreachable', message: 'Rustybin could not be reached: try again.' };
  }
  let json = null;
  try { json = await resp.json(); } catch { json = null; }
  if (resp.ok && json && json.portal_only) {
    if (json.token) {
      fetch(server + '/_rustybin/logout', { method: 'POST', headers: { Authorization: 'Bearer ' + json.token }, credentials: 'omit', keepalive: true }).catch(() => {});
    }
    return { ok: false, status: resp.status, reason: 'portal_only', portalUrl: server + (json.portal_url || '/portal/'), portalSignedIn: !!json.portal_signed_in, message: 'This account opens the API portal, not the console.' };
  }
  if (resp.ok && json && json.token) {
    setToken(json.token, !!remember);
    return { ok: true, status: resp.status, json };
  }
  const reason = (json && json.reason) || (resp.status === 429 ? 'rate_limited' : 'invalid_token');
  const retryAfter = Number(resp.headers.get('Retry-After') || (json && json.retry_after) || 0) || 0;
  return { ok: false, status: resp.status, reason, retryAfter, message: (json && json.error) || ('Sign-in failed (' + resp.status + ').') };
}

// ── Single sign-on (password mode with sso.* set up) ──
//
// The button starts GET /_rustybin/sso/start with the S256 challenge of a
// verifier kept in this tab; the provider sends the browser back through the
// instance's callback to #/sso/<code> (or #/sso-error/<reason>), and POST
// /_rustybin/sso/token trades the code and the verifier for a sign-in token.

const SSO_VERIFIER_KEY = 'rustybin.sso.verifier';
const SSO_REMEMBER_KEY = 'rustybin.sso.remember';

/** GET /_rustybin/sso/status (open): {enabled, ready, name, password_sign_in}, or null. */
export async function ssoStatus(server) {
  if (mode !== 'password') return null;
  try {
    const r = await fetch(server + '/_rustybin/sso/status', { headers: { Accept: 'application/json' }, credentials: 'omit', cache: 'no-store' });
    if (!r.ok) return null;
    return await r.json();
  } catch {
    return null;
  }
}

/** Go to the identity provider (the page leaves). */
export function startSso(server, remember) {
  const verifier = randomString(32);
  try {
    sessionStorage.setItem(SSO_VERIFIER_KEY, verifier);
    sessionStorage.setItem(SSO_REMEMBER_KEY, remember ? '1' : '');
  } catch { /* the sign-in cannot finish without storage: the server says so */ }
  location.assign(server + '/_rustybin/sso/start?return_to=console&challenge=' + pkceChallenge(verifier));
}

/**
 * The provider's answer in the address bar: {code} for #/sso/<code>,
 * {error} for #/sso-error/<reason>, null otherwise. Stripped at once.
 */
export function takeSsoFragment() {
  const m = /^#\/sso(-error)?\/([A-Za-z0-9_-]+)$/.exec(location.hash || '');
  if (!m) return null;
  try { history.replaceState(null, '', location.pathname + location.search); } catch { location.hash = ''; }
  return m[1] ? { error: m[2] } : { code: m[2] };
}

/** Trade the code for a sign-in token. Resolves {ok} or {ok: false, reason, message}. */
export async function finishSso(server, code) {
  let verifier = '';
  let remember = false;
  try {
    verifier = sessionStorage.getItem(SSO_VERIFIER_KEY) || '';
    remember = sessionStorage.getItem(SSO_REMEMBER_KEY) === '1';
    sessionStorage.removeItem(SSO_VERIFIER_KEY);
    sessionStorage.removeItem(SSO_REMEMBER_KEY);
  } catch { /* no storage */ }
  if (!verifier) return { ok: false, reason: 'invalid_code', message: 'This tab did not start the sign-in: start again.' };
  let resp;
  let json = null;
  try {
    resp = await fetch(server + '/_rustybin/sso/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ code, verifier }),
      credentials: 'omit',
      cache: 'no-store',
    });
    try { json = await resp.json(); } catch { json = null; }
  } catch {
    return { ok: false, reason: 'unreachable', message: 'Rustybin could not be reached: try again.' };
  }
  if (resp.ok && json && json.token) {
    setToken(json.token, remember);
    return { ok: true, json };
  }
  return { ok: false, reason: (json && json.reason) || 'invalid_code', message: (json && json.error) || ('The sign-in failed (' + resp.status + ').') };
}

/** True when the token came from single sign-on (claim rb_via "sso"). */
export function signedInWithSso() {
  const c = claims();
  return !!(c && c.rb_use === 'console' && c.rb_via === 'sso');
}

/** Show a refusal on the sign-in screen (a reason the screen has a text for). */
export function showReason(why) {
  reason = why || 'invalid_token';
  emit('required');
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
  // A single sign-on refusal stays on the screen until the next sign-in
  // (the views' first calls answer 401 too).
  if (!token && reason.startsWith('sso_')) return;
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

/** Subscribe to 'token' (signed in), 'required' (sign-in needed) and 'reset' (forget the previous user's data) events. */
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

// A stored JWT may have expired while the tab was closed.
if (token) scheduleExpiry();
identity = identityOf(token);
if (required() && !token) reason = 'missing';
