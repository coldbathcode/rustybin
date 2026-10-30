// HTTP helpers: a fetch wrapper that never throws and always times out,
// streaming readers (SSE, NDJSON), base URL handling and curl rendering.
import * as settings from './settings.js';
import * as auth from './auth.js';
import * as visit from './visit.js';

/** Base URL of this Rustybin instance, keeping a gateway path prefix
 *  (`https://gw/prefix/ui/` -> `https://gw/prefix`). */
export const SERVER = (() => {
  const p = location.pathname;
  const i = p.indexOf('/ui/');
  const prefix = i >= 0 ? p.slice(0, i) : p.replace(/\/ui$/, '').replace(/\/$/, '');
  return location.origin + prefix;
})();

export const SESSION_HEADER = 'X-Rustybin-Session';

/**
 * True for URLs of this server's mock management API (`/_rustybin/mocks*`)
 * and the portal's control routes (`/_rustybin/portal*`, where visitors
 * publish their own mocks): the visitor's manage tokens go there.
 */
export function isMocksUrl(u) {
  try {
    const url = new URL(u, location.href);
    const base = new URL(SERVER + '/');
    const root = base.pathname.replace(/\/$/, '');
    return url.origin === base.origin && ['/_rustybin/mocks', '/_rustybin/portal'].some((p) => url.pathname === root + p || url.pathname.startsWith(root + p + '/'));
  } catch {
    return false;
  }
}

/** Header carrying a token bound to an API key (shared mode, data plane). */
export const KEY_TOKEN_HEADER = 'X-Rustybin-Key-Token';

/** True for URLs of this server (not a gateway or another host). */
export function isServerUrl(u) {
  try {
    const url = new URL(u, location.href);
    const base = new URL(SERVER + '/');
    const prefix = base.pathname.replace(/\/$/, '');
    return url.origin === base.origin && (prefix === '' || url.pathname === prefix || url.pathname.startsWith(prefix + '/'));
  } catch {
    return false;
  }
}

/** True for URLs of this server's control plane (`/_rustybin` and below). */
export function isControlUrl(u) {
  try {
    const url = new URL(u, location.href);
    const base = new URL(SERVER + '/');
    const prefix = base.pathname.replace(/\/$/, '') + '/_rustybin';
    return url.origin === base.origin && (url.pathname === prefix || url.pathname.startsWith(prefix + '/'));
  } catch {
    return false;
  }
}

export function trimBase(u) {
  return String(u || '').trim().replace(/\/+$/, '');
}

/** The base for a request: the gateway URL when `viaGateway`, else this server. */
export function baseFor(viaGateway) {
  const gw = trimBase(settings.get('gatewayUrl'));
  return viaGateway && gw ? gw : SERVER;
}

export function isCrossOrigin(u) {
  try { return new URL(u, location.href).origin !== location.origin; } catch { return false; }
}

function hasHeader(headers, name) {
  const n = name.toLowerCase();
  return headers.some(([k]) => k.toLowerCase() === n);
}

/** Normalise headers to an ordered list of [name, value]. */
export function headerList(headers) {
  if (!headers) return [];
  if (Array.isArray(headers)) return headers.filter(([k]) => k && String(k).trim()).map(([k, v]) => [String(k).trim(), String(v ?? '')]);
  return Object.entries(headers).map(([k, v]) => [k, String(v ?? '')]);
}

/**
 * Add the console session / admin headers when configured, and the
 * control-plane token (`Authorization: Bearer`) on calls to this server's
 * `/_rustybin/*` routes.
 */
export function withConsoleHeaders(list, opts = {}) {
  const out = list.slice();
  const session = settings.get('session');
  if (opts.session !== false && session && !hasHeader(out, SESSION_HEADER)) out.push([SESSION_HEADER, session]);
  const control = auth.getToken();
  if (control && opts.url && isControlUrl(opts.url) && !hasHeader(out, 'authorization')) out.push(['Authorization', 'Bearer ' + control]);
  // Signed in for a shared-mode key: this server's data plane needs the key,
  // which the console never holds; the token bound to it stands in.
  if (control && auth.boundKey() && opts.url && isServerUrl(opts.url) && !isControlUrl(opts.url)
    && !hasHeader(out, 'x-rustybin-key') && !hasHeader(out, KEY_TOKEN_HEADER)) out.push([KEY_TOKEN_HEADER, control]);
  // Self-service: the visitor's manage tokens, to this server's mock API only.
  const mockTokens = visit.tokens();
  if (mockTokens.length && opts.url && isMocksUrl(opts.url) && !hasHeader(out, visit.HEADER)) out.push([visit.HEADER, mockTokens.join(', ')]);
  // Admin actions: the configured admin token, else the control token (the
  // admin token itself in token mode, or a JWT that may carry the admin scope).
  const admin = settings.get('adminToken') || (auth.required() ? control : '');
  if (opts.admin && admin && !hasHeader(out, 'x-rustybin-admin-token')) out.push(['X-Rustybin-Admin-Token', admin]);
  return out;
}

export function explainNetworkError(url, err, timeoutMs, timedOut) {
  if (timedOut) return `No response after ${Math.round(timeoutMs / 1000)} s (timed out).`;
  if (err && err.name === 'AbortError') return 'Request cancelled.';
  if (isCrossOrigin(url)) {
    return `The browser could not read the response from ${new URL(url, location.href).origin}. Either it is unreachable, ` +
      'or it does not allow cross-origin requests from this console (CORS: it must answer the preflight and send ' +
      `Access-Control-Allow-Origin for ${location.origin}). Use "Copy as curl" to send the same request from a terminal.`;
  }
  return 'Network error: the request did not reach Rustybin (is the server still running?).';
}

/**
 * Send a request. Never throws: resolves to
 * {ok, status, statusText, headers: [[k, v]], header(name), text, json, ms, url, error, contentType, request},
 * where request.headers is what was sent and request.shown the same with console credentials masked.
 */
/** Headers the console adds on its own that carry a credential. */
const CREDENTIAL_HEADERS = ['authorization', 'x-rustybin-admin-token', KEY_TOKEN_HEADER.toLowerCase(), visit.HEADER.toLowerCase()];

/** A credential for the screen: its scheme (`Bearer `), then the first 4 and last 2 characters. */
export function maskCredential(value) {
  const v = String(value);
  const m = /^([A-Za-z]+\s+)(\S.*)$/.exec(v);
  const [scheme, secret] = m ? [m[1], m[2]] : ['', v];
  return scheme + (secret.length > 8 ? secret.slice(0, 4) + '...' + secret.slice(-2) : '...');
}

export async function send(req) {
  const method = (req.method || 'GET').toUpperCase();
  const url = req.url;
  const timeoutMs = req.timeout ?? 30000;
  const own = headerList(req.headers);
  const headers = withConsoleHeaders(own, req);
  // Credentials the console added itself (control token, admin token, manage
  // tokens) go out as they are but are shown masked: the console may be on a projector.
  const shown = headers.map(([k, v], i) => (i >= own.length && CREDENTIAL_HEADERS.includes(k.toLowerCase()) ? [k, maskCredential(v)] : [k, v]));
  const ctrl = new AbortController();
  let timedOut = false;
  const timer = setTimeout(() => { timedOut = true; ctrl.abort(); }, timeoutMs);
  const onAbort = () => ctrl.abort();
  if (req.signal) req.signal.addEventListener('abort', onAbort);
  const request = { method, url, headers, shown, body: req.body ?? null };
  const started = performance.now();
  const result = { ok: false, status: 0, statusText: '', headers: [], text: '', json: undefined, ms: 0, url, error: null, contentType: '', request };
  result.header = (name) => {
    const n = name.toLowerCase();
    const hit = result.headers.find(([k]) => k === n);
    return hit ? hit[1] : null;
  };
  try {
    const init = { method, headers, signal: ctrl.signal, redirect: req.redirect || 'follow', cache: 'no-store' };
    if (req.body !== undefined && req.body !== null && method !== 'GET' && method !== 'HEAD') init.body = req.body;
    const resp = await fetch(url, init);
    result.status = resp.status;
    result.statusText = resp.statusText;
    result.ok = resp.ok;
    result.headers = Array.from(resp.headers.entries());
    result.contentType = resp.headers.get('content-type') || '';
    result.ttfbMs = performance.now() - started;
    if (req.stream) {
      if (resp.status === 401 && isControlUrl(url)) auth.markUnauthorized();
      result.response = resp;
      return result;
    }
    result.text = await resp.text();
    if (result.contentType.includes('json') || /^\s*[{[]/.test(result.text)) {
      try { result.json = JSON.parse(result.text); } catch { /* not JSON */ }
    }
    if (resp.status === 401 && isControlUrl(url)) auth.markUnauthorized(result.json && result.json.reason);
  } catch (err) {
    result.error = explainNetworkError(url, err, timeoutMs, timedOut);
  } finally {
    clearTimeout(timer);
    // Streams stay cancellable through the caller's signal.
    if (req.signal && !result.response) req.signal.removeEventListener('abort', onAbort);
    result.ms = performance.now() - started;
  }
  return result;
}

/** GET JSON from this server; resolves to the parsed body or throws an Error with a readable message. */
export async function getJson(path, opts = {}) {
  const r = await send(Object.assign({ url: SERVER + path, timeout: 15000 }, opts));
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}: ${(r.json && (r.json.error || r.json.message)) || r.text.slice(0, 200)}`);
  if (r.json === undefined) throw new Error('Expected JSON from ' + path);
  return r.json;
}

/** Read a streaming body chunk by chunk as text. */
export async function readText(response, onText, signal) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  // cancel() returns a promise that rejects once the fetch was aborted.
  const stop = () => { try { reader.cancel().catch(() => {}); } catch { /* ignore */ } };
  if (signal) signal.addEventListener('abort', stop);
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      onText(decoder.decode(value, { stream: true }));
    }
    const tail = decoder.decode();
    if (tail) onText(tail);
  } finally {
    if (signal) signal.removeEventListener('abort', stop);
  }
}

/** Incremental Server-Sent Events parser: feed(text) calls onEvent({event, data, id, raw}). */
export function sseParser(onEvent) {
  let buf = '';
  return (chunk) => {
    buf += chunk;
    buf = buf.replace(/\r\n?/g, '\n');
    let idx;
    while ((idx = buf.indexOf('\n\n')) >= 0) {
      const block = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      if (!block.trim()) continue;
      let event = 'message', id = null;
      const data = [];
      for (const line of block.split('\n')) {
        if (line.startsWith(':')) continue;
        const c = line.indexOf(':');
        const field = c < 0 ? line : line.slice(0, c);
        let val = c < 0 ? '' : line.slice(c + 1);
        if (val.startsWith(' ')) val = val.slice(1);
        if (field === 'event') event = val;
        else if (field === 'data') data.push(val);
        else if (field === 'id') id = val;
      }
      if (!data.length && event === 'message') continue;
      onEvent({ event, data: data.join('\n'), id, raw: block });
    }
  };
}

/** Incremental NDJSON parser: feed(text) calls onLine(line). */
export function ndjsonParser(onLine) {
  let buf = '';
  return (chunk) => {
    buf += chunk;
    let idx;
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx).trim();
      buf = buf.slice(idx + 1);
      if (line) onLine(line);
    }
  };
}

function shellQuote(s) {
  return "'" + String(s).replace(/'/g, "'\\''") + "'";
}

/** curl command for a request ({method, url, headers: [[k, v]], body}). */
export function toCurl(req) {
  const parts = ['curl'];
  const method = (req.method || 'GET').toUpperCase();
  if (req.stream) parts.push('-N');
  if (method !== 'GET' || (req.body && method === 'GET')) parts.push('-X ' + method);
  for (const [k, v] of headerList(req.headers)) parts.push('-H ' + shellQuote(`${k}: ${v}`));
  if (req.body !== undefined && req.body !== null && req.body !== '') parts.push('--data-raw ' + shellQuote(req.body));
  parts.push(shellQuote(new URL(req.url, location.href).href));
  return parts.join(' \\\n  ');
}

/** Status class label: 2xx, 3xx, ... */
export function statusClass(code) {
  const c = Number(code) || 0;
  return c ? Math.floor(c / 100) + 'xx' : 'error';
}
