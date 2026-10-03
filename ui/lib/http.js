// HTTP helpers: a fetch wrapper that never throws and always times out,
// streaming readers (SSE, NDJSON), base URL handling and curl rendering.
import * as settings from './settings.js';

/** Base URL of this Rustybin instance, keeping a gateway path prefix
 *  (`https://gw/prefix/ui/` -> `https://gw/prefix`). */
export const SERVER = (() => {
  const p = location.pathname;
  const i = p.indexOf('/ui/');
  const prefix = i >= 0 ? p.slice(0, i) : p.replace(/\/ui$/, '').replace(/\/$/, '');
  return location.origin + prefix;
})();

export const SESSION_HEADER = 'X-Rustybin-Session';

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

/** Add the console session / admin headers when configured. */
export function withConsoleHeaders(list, opts = {}) {
  const out = list.slice();
  const session = settings.get('session');
  if (opts.session !== false && session && !hasHeader(out, SESSION_HEADER)) out.push([SESSION_HEADER, session]);
  const admin = settings.get('adminToken');
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
 * {ok, status, statusText, headers: [[k, v]], header(name), text, json, ms, url, error, contentType, request}.
 */
export async function send(req) {
  const method = (req.method || 'GET').toUpperCase();
  const url = req.url;
  const timeoutMs = req.timeout ?? 30000;
  const headers = withConsoleHeaders(headerList(req.headers), req);
  const ctrl = new AbortController();
  let timedOut = false;
  const timer = setTimeout(() => { timedOut = true; ctrl.abort(); }, timeoutMs);
  const onAbort = () => ctrl.abort();
  if (req.signal) req.signal.addEventListener('abort', onAbort);
  const request = { method, url, headers, body: req.body ?? null };
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
      result.response = resp;
      return result;
    }
    result.text = await resp.text();
    if (result.contentType.includes('json') || /^\s*[{[]/.test(result.text)) {
      try { result.json = JSON.parse(result.text); } catch { /* not JSON */ }
    }
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
  const stop = () => { try { reader.cancel(); } catch { /* ignore */ } };
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
