// An MCP client for the console's inspector. Transports: Streamable HTTP
// 2026-07-28 (stateless), Streamable HTTP with sessions (2025-11-25,
// 2025-06-18) and the legacy HTTP+SSE pair (2024-11-05). Modes: `browser`
// (fetch from this page; needs CORS from the server) and `relay` (through
// POST /_rustybin/mcp-relay, which forwards the same messages from the
// instance under the egress policy). Every message in and out goes to the
// `log` callback with the HTTP headers that carried it.
import { send, sseParser, readText, SERVER } from './http.js';
import { utf8, toBase64 } from './crypto.js';

export const MODERN = '2026-07-28';
export const VERSIONS = [
  ['2026-07-28', '2026-07-28 stateless', 'modern'],
  ['2025-11-25', '2025-11-25 session', 'session'],
  ['2025-06-18', '2025-06-18 session', 'session'],
  ['2024-11-05', '2024-11-05 legacy HTTP+SSE', 'legacy'],
];

/** The transport of a version: modern, session or legacy. */
export function transportOf(version) {
  const v = VERSIONS.find((x) => x[0] === version);
  return v ? v[2] : 'modern';
}

export function headerSafe(v) {
  const s = String(v);
  if (/^[\x21-\x7e](?:[\x20-\x7e]*[\x21-\x7e])?$/.test(s)) return s;
  return '=?base64?' + toBase64(utf8(s)) + '?=';
}

/** `key="value"` pairs of a WWW-Authenticate challenge. */
export function parseChallenge(value) {
  const out = {};
  if (!value) return out;
  const re = /(\w+)="([^"]*)"/g;
  let m;
  while ((m = re.exec(value))) out[m[1]] = m[2];
  return out;
}

function header(list, name) {
  const n = name.toLowerCase();
  const hit = (list || []).find(([k]) => String(k).toLowerCase() === n);
  return hit ? hit[1] : null;
}

/** Parse the SSE body of a relay answer: {status, headers, messages, body, error, session}. */
async function readRelay(r, onMessage, signal) {
  const out = { status: 0, headers: [], messages: [], body: null, error: null, session: null };
  const feed = sseParser((ev) => {
    let data = null;
    try { data = JSON.parse(ev.data); } catch { data = null; }
    if (ev.event === 'relay' && data) {
      out.status = data.status;
      out.session = data.session || null;
      out.headers = Object.entries(data.headers || {});
    } else if (ev.event === 'message' && data) {
      out.messages.push(data);
      if (onMessage) onMessage(data);
    } else if (ev.event === 'body' && data) {
      out.body = data.text;
    } else if (ev.event === 'error' && data) {
      out.error = data.error;
    }
  });
  await readText(r.response, feed, signal);
  return out;
}

export class McpClient {
  /**
   * opts: {url (absolute URL, or a path on this instance), mode: browser |
   * relay, version, authHeaders: [[k, v]], connection (a saved connection id,
   * relay only), log(dir, json, extra), onServerRequest(msg) -> {result} |
   * {error}}.
   */
  constructor(opts) {
    this.url = opts.url;
    this.mode = opts.mode || 'browser';
    this.version = opts.version || MODERN;
    this.transport = transportOf(this.version);
    this.authHeaders = opts.authHeaders || [];
    this.connection = opts.connection || null;
    this.log = opts.log || (() => {});
    this.onServerRequest = opts.onServerRequest || (async () => ({ error: { code: -32601, message: 'not supported' } }));
    this.sessionId = null;
    this.relaySession = null;
    this.seq = 0;
    this.toolSchemas = new Map();
    this.initialized = false;
    this.latency = [];
    // Legacy (browser mode): the open stream, the POST endpoint, pending requests.
    this.legacy = null;
  }

  get modern() { return this.transport === 'modern'; }

  /** The absolute URL for browser mode (paths resolve against this instance). */
  absolute(u) {
    return /^https?:\/\//i.test(u) ? u : SERVER + u;
  }

  baseHeaders(method, params) {
    const hs = [['Content-Type', 'application/json'], ['Accept', 'application/json, text/event-stream'], ...this.authHeaders];
    if (this.modern) {
      hs.push(['MCP-Protocol-Version', MODERN]);
      if (method) {
        hs.push(['Mcp-Method', method]);
        const p = params || {};
        if ((method === 'tools/call' || method === 'prompts/get') && p.name) hs.push(['Mcp-Name', headerSafe(p.name)]);
        if (method === 'resources/read' && p.uri) hs.push(['Mcp-Name', headerSafe(p.uri)]);
        if (method === 'tools/call') {
          const schema = this.toolSchemas.get(p.name);
          for (const [prop, spec] of Object.entries((schema && schema.properties) || {})) {
            const token = spec && spec['x-mcp-header'];
            const v = p.arguments && p.arguments[prop];
            if (token && v !== undefined && v !== null && typeof v !== 'object') hs.push(['Mcp-Param-' + token, headerSafe(v)]);
          }
        }
      }
    } else if (this.transport === 'session') {
      if (this.sessionId) hs.push(['Mcp-Session-Id', this.sessionId]);
      if (this.initialized && this.version !== '2025-03-26') hs.push(['MCP-Protocol-Version', this.version]);
    }
    return hs;
  }

  meta(extra) {
    if (!this.modern) return extra && Object.keys(extra).length ? extra : undefined;
    return Object.assign({ 'io.modelcontextprotocol/protocolVersion': MODERN, 'io.modelcontextprotocol/clientCapabilities': { elicitation: {}, sampling: {} } }, extra || {});
  }

  /**
   * POST one message; resolves {status, headers, messages, httpBody, error}.
   * `onMessage` sees messages as they arrive (progress, server requests).
   */
  async post(msg, headers, opts = {}) {
    const started = performance.now();
    let out;
    if (this.mode === 'relay') {
      const hmap = {};
      for (const [k, v] of headers) hmap[k] = v;
      const body = { target: this.url, transport: this.transport === 'legacy' ? 'legacy' : 'streamable', headers: hmap, message: msg };
      if (this.relaySession) body.session = this.relaySession;
      if (this.connection) body.connection = this.connection;
      const r = await send({ method: 'POST', url: SERVER + '/_rustybin/mcp-relay', headers: [['Content-Type', 'application/json'], ['Accept', 'text/event-stream']], body: JSON.stringify(body), session: false, stream: true, timeout: 60000, signal: opts.signal });
      if (r.error || !r.ok) {
        let detail = r.error;
        let reason = null;
        if (!detail && r.response) {
          try { const j = await r.response.json(); detail = j.error; reason = j.reason; } catch { detail = 'HTTP ' + r.status; }
        }
        out = { status: r.status, headers: [], messages: [], httpBody: null, error: { message: 'relay: ' + (detail || 'HTTP ' + r.status), reason } };
      } else {
        const rel = await readRelay(r, opts.onMessage, opts.signal);
        if (rel.session) this.relaySession = rel.session;
        out = { status: rel.status, headers: rel.headers, messages: rel.messages, httpBody: rel.body, error: rel.error ? { message: rel.error } : null };
      }
    } else if (this.transport === 'legacy') {
      out = await this.legacyPost(msg, opts);
    } else {
      const r = await send({ method: 'POST', url: this.absolute(this.url), headers, body: JSON.stringify(msg), stream: true, timeout: 60000, signal: opts.signal });
      out = { status: r.status, headers: r.headers, messages: [], httpBody: null, error: null };
      if (r.error) { out.error = { message: r.error }; }
      else {
        const ct = r.contentType || '';
        try {
          if (ct.includes('event-stream')) {
            await readText(r.response, sseParser((ev) => {
              let m = null;
              try { m = JSON.parse(ev.data); } catch { m = null; }
              if (m) { out.messages.push(m); if (opts.onMessage) opts.onMessage(m); }
            }), opts.signal);
          } else {
            const text = await r.response.text();
            let parsed = null;
            try { parsed = JSON.parse(text); } catch { parsed = null; }
            const list = Array.isArray(parsed) ? parsed : parsed && typeof parsed === 'object' ? [parsed] : [];
            if (list.some((m) => m && (m.jsonrpc || m.result !== undefined || m.error !== undefined))) {
              for (const m of list) { out.messages.push(m); if (opts.onMessage) opts.onMessage(m); }
            } else if (text) out.httpBody = text.slice(0, 16000);
          }
        } catch (e) {
          out.error = { message: opts.signal && opts.signal.aborted ? 'cancelled' : String(e.message || e) };
        }
      }
    }
    const sid = header(out.headers, 'mcp-session-id');
    if (sid) this.sessionId = sid;
    out.ms = performance.now() - started;
    this.latency.push(out.ms);
    if (this.latency.length > 200) this.latency.shift();
    return out;
  }

  // ── Legacy HTTP+SSE in the browser ──

  async legacyOpen() {
    if (this.legacy) return this.legacy;
    const st = { endpoint: null, pending: new Map(), ctrl: new AbortController(), ready: null, closed: false };
    this.legacy = st;
    st.ready = new Promise((resolve, reject) => {
      (async () => {
        const r = await send({ method: 'GET', url: this.absolute(this.url), headers: [['Accept', 'text/event-stream'], ...this.authHeaders], stream: true, timeout: 600000, signal: st.ctrl.signal });
        if (r.error || !r.ok) { reject(new Error(r.error || ('the SSE stream answered ' + r.status))); return; }
        this.log('note', { stream: 'opened', status: r.status }, { url: this.absolute(this.url) });
        try {
          await readText(r.response, sseParser((ev) => {
            if (ev.event === 'endpoint') {
              st.endpoint = new URL(ev.data.trim(), this.absolute(this.url)).toString();
              resolve(st);
              return;
            }
            let m = null;
            try { m = JSON.parse(ev.data); } catch { m = null; }
            if (!m) return;
            const p = m.id !== undefined && (m.result !== undefined || m.error !== undefined) ? st.pending.get(m.id) : null;
            if (p) p.messages.push(m);
            else if (st.current) st.current.messages.push(m);
            if (st.current && st.current.onMessage) st.current.onMessage(m);
            if (p) { st.pending.delete(m.id); p.resolve(); }
          }), st.ctrl.signal);
        } catch { /* stream ended */ }
        st.closed = true;
        reject(new Error('the SSE stream ended'));
      })();
    });
    return st.ready;
  }

  async legacyPost(msg, opts) {
    const out = { status: 0, headers: [], messages: [], httpBody: null, error: null };
    let st;
    try { st = await this.legacyOpen(); } catch (e) { out.error = { message: String(e.message || e) }; this.legacy = null; return out; }
    const entry = { messages: [], onMessage: opts.onMessage, resolve: null };
    const done = new Promise((resolve) => { entry.resolve = resolve; });
    const expects = msg.id !== undefined && msg.method;
    if (expects) st.pending.set(msg.id, entry);
    st.current = entry;
    const r = await send({ method: 'POST', url: st.endpoint, headers: [['Content-Type', 'application/json'], ...this.authHeaders], body: JSON.stringify(msg), timeout: 30000 });
    out.status = r.status;
    out.headers = r.headers;
    if (r.error || r.status >= 400) { out.error = { message: r.error || ('the endpoint answered ' + r.status) }; st.pending.delete(msg.id); return out; }
    if (expects) {
      const timer = new Promise((resolve) => setTimeout(resolve, 30000));
      await Promise.race([done, timer]);
      if (st.pending.has(msg.id)) { st.pending.delete(msg.id); out.error = { message: 'no answer on the stream within 30 s' }; }
    }
    out.messages = entry.messages;
    st.current = null;
    return out;
  }

  // ── JSON-RPC ──

  /** Send a request; resolves {result, error, status, headers, httpBody, ms, request}. */
  async request(method, params, opts = {}) {
    const id = ++this.seq;
    params = Object.assign({}, params || {});
    const meta = this.meta(opts.progressToken ? { progressToken: opts.progressToken } : null);
    if (meta) params._meta = meta;
    const msg = { jsonrpc: '2.0', id, method, params };
    const headers = this.baseHeaders(method, params);
    this.log('out', msg, { url: this.url, headers, mode: this.mode });
    const result = { status: 0, headers: [], error: null, result: undefined, request: { method: 'POST', url: this.absolute(this.url), headers, body: JSON.stringify(msg) } };
    const replies = [];
    const onMessage = (m) => {
      if (m.id === id && ('result' in m || 'error' in m)) return;
      if (m.method && m.id !== undefined) replies.push(m);
      else if (m.method && opts.onNotification) { this.log('in', m, { note: 'notification' }); opts.onNotification(m); }
    };
    const r = await this.post(msg, headers, { signal: opts.signal, onMessage });
    result.status = r.status;
    result.headers = r.headers;
    result.httpBody = r.httpBody;
    result.ms = r.ms;
    // Server requests (elicitation, sampling) in the stream: answer them.
    for (const m of replies) {
      this.log('in', m, { status: r.status, note: 'server request' });
      const reply = await this.onServerRequest(m);
      const resp = { jsonrpc: '2.0', id: m.id, ...(reply.error ? { error: reply.error } : { result: reply.result }) };
      this.log('out', resp, { note: 'reply' });
      await this.post(resp, this.baseHeaders(null), {});
    }
    for (const m of r.messages) {
      if (m.id === id && ('result' in m || 'error' in m)) {
        this.log('in', m, { status: r.status, ms: Math.round(r.ms) });
        if (m.error) result.error = m.error; else result.result = m.result;
      }
    }
    if (r.error && result.result === undefined && !result.error) { result.error = r.error; this.log('note', { error: r.error.message }); }
    if (result.result === undefined && !result.error) {
      result.error = { message: r.status >= 400 ? `HTTP ${r.status}` : `No JSON-RPC response (HTTP ${r.status})`, data: r.httpBody || undefined };
      this.log('note', { http: r.status, body: r.httpBody ? r.httpBody.slice(0, 500) : undefined });
    }
    return result;
  }

  async notify(method, params) {
    const msg = { jsonrpc: '2.0', method };
    if (params) msg.params = params;
    this.log('out', msg, { mode: this.mode });
    const r = await this.post(msg, this.baseHeaders(method, params), {});
    this.log('note', { http: r.status, for: method });
    return r;
  }

  async connect() {
    if (this.modern) return this.request('server/discover', {});
    const res = await this.request('initialize', { protocolVersion: this.version, capabilities: { elicitation: {}, sampling: {} }, clientInfo: { name: 'rustybin-console', version: '1.0.0' } });
    if (res.result) {
      this.initialized = true;
      await this.notify('notifications/initialized');
    }
    return res;
  }

  /** End the session (DELETE, or the relay session), close a legacy stream. */
  async close() {
    if (this.mode === 'relay' && this.relaySession) {
      this.log('note', { action: 'end relay session', session: this.relaySession });
      await send({ method: 'DELETE', url: SERVER + '/_rustybin/mcp-relay/' + encodeURIComponent(this.relaySession), session: false, timeout: 10000 });
      this.relaySession = null;
    } else if (this.transport === 'session' && this.sessionId) {
      this.log('note', { action: 'DELETE session', session: this.sessionId });
      await send({ method: 'DELETE', url: this.absolute(this.url), headers: this.baseHeaders(), timeout: 10000 });
    }
    if (this.legacy) { this.legacy.ctrl.abort(); this.legacy = null; }
    this.sessionId = null;
    this.initialized = false;
  }

  /** Median and worst latency of this connection's calls. */
  stats() {
    const v = this.latency.slice().sort((a, b) => a - b);
    if (!v.length) return null;
    return { calls: v.length, p50: v[Math.floor((v.length - 1) / 2)], max: v[v.length - 1] };
  }
}

// ── Exports of a session ───────────────────────────────────────────

function shq(s) { return "'" + String(s).replace(/'/g, "'\\''") + "'"; }

/** The outgoing messages of a log as Hurl, curl, a Python SDK script and claude mcp add. */
export function exportSession(entries, opts) {
  const out = entries.filter((e) => e.dir === 'out' && e.json && e.json.method && e.extra && e.extra.headers);
  const url = /^https?:\/\//i.test(opts.url) ? opts.url : SERVER + opts.url;
  const hurl = out.map((e) => ['POST ' + url, ...e.extra.headers.map(([k, v]) => `${k}: ${v}`), JSON.stringify(e.json, null, 2), 'HTTP *'].join('\n')).join('\n\n');
  const curl = out.map((e) => ['curl -sS -X POST ' + shq(url), ...e.extra.headers.map(([k, v]) => '  -H ' + shq(`${k}: ${v}`)), '  --data ' + shq(JSON.stringify(e.json))].join(' \\\n')).join('\n\n');
  const authLines = (opts.authHeaders || []).map(([k, v]) => `    ${JSON.stringify(k)}: ${JSON.stringify(v)},`).join('\n');
  const python = [
    '# pip install mcp',
    'import asyncio',
    'from mcp import ClientSession',
    opts.transport === 'legacy' ? 'from mcp.client.sse import sse_client' : 'from mcp.client.streamable_http import streamablehttp_client',
    '',
    'HEADERS = {',
    authLines,
    '}',
    '',
    'async def main():',
    opts.transport === 'legacy'
      ? `    async with sse_client(${JSON.stringify(url)}, headers=HEADERS) as (read, write):`
      : `    async with streamablehttp_client(${JSON.stringify(url)}, headers=HEADERS) as (read, write, _):`,
    '        async with ClientSession(read, write) as session:',
    '            await session.initialize()',
    '            tools = await session.list_tools()',
    '            print([t.name for t in tools.tools])',
    '',
    'asyncio.run(main())',
  ].filter((l) => l !== null).join('\n');
  let claude = 'claude mcp add --transport ' + (opts.transport === 'legacy' ? 'sse' : 'http') + ' ' + (opts.name || 'rustybin') + ' ' + shq(url);
  for (const [k, v] of opts.authHeaders || []) claude += ' --header ' + shq(`${k}: ${v}`);
  return { hurl, curl, python, claude };
}
