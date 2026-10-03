// MCP inspector: connect to the mock MCP server (2026-07-28 stateless or the
// 2025-11-25 session handshake), list and call tools with forms generated
// from their input schemas, follow progress notifications, read resources,
// get prompts, answer elicitation / sampling requests, and log every
// JSON-RPC message. /mcp/protected shows the OAuth discovery steps.
import { h, replace, clear, card, field, input, select, notice, empty, icon, badge, statusBadge, fmtMs, fmtTime, segmented, tabs, spinner, toast, kvgrid } from '../lib/dom.js';
import { jsonView, codeBlock, headersTable } from '../lib/format.js';
import { send, sseParser, readText, toCurl } from '../lib/http.js';
import { targetPicker } from '../lib/widgets.js';
import { schemaForm, promptArgsSchema } from '../lib/schemaform.js';
import { decodeJwt, utf8, toBase64 } from '../lib/crypto.js';
import * as settings from '../lib/settings.js';

const MODERN = '2026-07-28';
const LEGACY = '2025-11-25';
const ENDPOINTS = [
  ['/mcp', '/mcp (open)'],
  ['/mcp/protected', '/mcp/protected (OAuth bearer)'],
  ['/mcp/apikey', '/mcp/apikey (X-API-Key)'],
  ['/mcp/servers/weather', '/mcp/servers/weather'],
  ['/mcp/servers/crm', '/mcp/servers/crm'],
  ['/mcp/servers/devtools', '/mcp/servers/devtools'],
];

function headerSafe(v) {
  const s = String(v);
  if (/^[\x21-\x7e](?:[\x20-\x7e]*[\x21-\x7e])?$/.test(s)) return s;
  return '=?base64?' + toBase64(utf8(s)) + '?=';
}

function parseChallenge(value) {
  const out = {};
  if (!value) return out;
  const re = /(\w+)="([^"]*)"/g;
  let m;
  while ((m = re.exec(value))) out[m[1]] = m[2];
  return out;
}

class McpClient {
  constructor(opts) {
    this.url = opts.url;
    this.version = opts.version;
    this.authHeaders = opts.authHeaders || [];
    this.log = opts.log;
    this.onServerRequest = opts.onServerRequest;
    this.sessionId = null;
    this.seq = 0;
    this.toolSchemas = new Map();
  }

  get modern() { return this.version === MODERN; }

  baseHeaders(method) {
    const hs = [['Content-Type', 'application/json'], ['Accept', 'application/json, text/event-stream'], ...this.authHeaders];
    if (this.modern) {
      hs.push(['MCP-Protocol-Version', MODERN]);
      if (method) hs.push(['Mcp-Method', method]);
    } else {
      if (this.sessionId) hs.push(['Mcp-Session-Id', this.sessionId]);
      if (this.initialized) hs.push(['MCP-Protocol-Version', LEGACY]);
    }
    return hs;
  }

  meta(extra) {
    if (!this.modern) return extra && Object.keys(extra).length ? extra : undefined;
    return Object.assign({ 'io.modelcontextprotocol/protocolVersion': MODERN, 'io.modelcontextprotocol/clientCapabilities': { elicitation: {}, sampling: {} } }, extra || {});
  }

  /** Send one request; resolves {result, error, status, headers, httpBody}. */
  async request(method, params, opts = {}) {
    const id = ++this.seq;
    params = Object.assign({}, params || {});
    const meta = this.meta(opts.progressToken ? { progressToken: opts.progressToken } : null);
    if (meta) params._meta = meta;
    const msg = { jsonrpc: '2.0', id, method, params };
    const headers = this.baseHeaders(method);
    if (this.modern) {
      if ((method === 'tools/call' || method === 'prompts/get') && params.name) headers.push(['Mcp-Name', headerSafe(params.name)]);
      if (method === 'resources/read' && params.uri) headers.push(['Mcp-Name', headerSafe(params.uri)]);
      if (method === 'tools/call') {
        const schema = this.toolSchemas.get(params.name);
        for (const [prop, spec] of Object.entries((schema && schema.properties) || {})) {
          const token = spec && spec['x-mcp-header'];
          const v = params.arguments && params.arguments[prop];
          if (token && v !== undefined && v !== null && typeof v !== 'object') headers.push(['Mcp-Param-' + token, headerSafe(v)]);
        }
      }
    }
    const body = JSON.stringify(msg);
    this.log('out', msg, { url: this.url, headers });
    const r = await send({ method: 'POST', url: this.url, headers, body, stream: true, timeout: 30000, signal: opts.signal });
    const out = { status: r.status, headers: r.headers, error: null, result: undefined, request: { method: 'POST', url: this.url, headers, body } };
    if (r.error) { out.error = { message: r.error }; this.log('note', { error: r.error }); return out; }
    const sid = r.header('mcp-session-id');
    if (sid) this.sessionId = sid;
    const ct = r.contentType;
    const handle = async (m) => {
      if (m.id === id && ('result' in m || 'error' in m)) {
        this.log('in', m, { status: r.status });
        if (m.error) out.error = m.error; else out.result = m.result;
        return;
      }
      if (m.method && m.id !== undefined) {
        this.log('in', m, { status: r.status, note: 'server request' });
        const reply = await this.onServerRequest(m);
        const resp = { jsonrpc: '2.0', id: m.id, ...(reply.error ? { error: reply.error } : { result: reply.result }) };
        this.log('out', resp, { note: 'reply' });
        await send({ method: 'POST', url: this.url, headers: this.baseHeaders(), body: JSON.stringify(resp), timeout: 15000 });
        return;
      }
      if (m.method) {
        this.log('in', m, { status: r.status });
        if (opts.onNotification) opts.onNotification(m);
      }
    };
    try {
      if (ct.includes('event-stream')) {
        const queue = [];
        const parse = sseParser((ev) => { try { queue.push(JSON.parse(ev.data)); } catch { /* ignore */ } });
        let chain = Promise.resolve();
        await readText(r.response, (chunk) => {
          parse(chunk);
          while (queue.length) { const m = queue.shift(); chain = chain.then(() => handle(m)); }
        }, opts.signal);
        await chain;
      } else {
        const text = await r.response.text();
        out.httpBody = text;
        if (text) {
          let parsed = null;
          try { parsed = JSON.parse(text); } catch { /* not JSON */ }
          if (parsed && (parsed.jsonrpc || parsed.id !== undefined)) {
            // Errors raised before the id was read come back with id null.
            if ((parsed.id === null || parsed.id === undefined) && ('error' in parsed || 'result' in parsed)) parsed.id = id;
            await handle(parsed);
          }
          else if (r.status >= 400) { out.error = { message: `HTTP ${r.status}`, data: parsed || text }; this.log('note', { http: r.status, body: parsed || text }); }
        } else if (r.status >= 400) {
          out.error = { message: `HTTP ${r.status}` };
          this.log('note', { http: r.status });
        }
      }
    } catch (e) {
      if (!out.error && out.result === undefined) out.error = { message: opts.signal && opts.signal.aborted ? 'cancelled' : String(e.message || e) };
    }
    if (out.result === undefined && !out.error) out.error = { message: `No JSON-RPC response (HTTP ${r.status})` };
    return out;
  }

  async notify(method, params) {
    const msg = { jsonrpc: '2.0', method };
    if (params) msg.params = params;
    this.log('out', msg);
    const r = await send({ method: 'POST', url: this.url, headers: this.baseHeaders(method), body: JSON.stringify(msg), timeout: 15000 });
    this.log('note', { http: r.status, for: method });
    return r;
  }

  async connect() {
    if (this.modern) return this.request('server/discover', {});
    const res = await this.request('initialize', { protocolVersion: LEGACY, capabilities: { elicitation: {}, sampling: {} }, clientInfo: { name: 'rustybin-console', version: '1.0.0' } });
    if (res.result) {
      this.initialized = true;
      await this.notify('notifications/initialized');
    }
    return res;
  }

  async close() {
    if (!this.modern && this.sessionId) {
      this.log('note', { action: 'DELETE session', session: this.sessionId });
      await send({ method: 'DELETE', url: this.url, headers: this.baseHeaders(), timeout: 10000 });
    }
    this.sessionId = null;
    this.initialized = false;
  }
}

export default {
  id: 'mcp',
  mount(root) {
    let alive = true;
    const st = settings.recall('mcp.state', { endpoint: '/mcp', version: MODERN, apiKey: 'demo-key', token: '', log: [] });
    settings.remember('mcp.state', st);
    let client = null;
    let server = null;
    let callCtrl = null;
    const target = targetPicker('mcp');

    const statusEl = h('div', { class: 'row' }, badge('not connected'));
    const authEl = h('div');
    const mainEl = h('div');
    const logEl = h('div', { class: 'log' });
    const pendingEl = h('div');
    const endpointSel = select(ENDPOINTS, st.endpoint, { 'aria-label': 'MCP endpoint' });
    const versionSeg = segmented([[MODERN, MODERN + ' stateless'], [LEGACY, LEGACY + ' session']], st.version, (v) => { st.version = v; }, 'Protocol version');
    const apiKeyIn = input({ value: st.apiKey, class: 'input mono' });
    apiKeyIn.addEventListener('input', () => { st.apiKey = apiKeyIn.value.trim(); });
    const tokenIn = input({ value: st.token, class: 'input mono', placeholder: 'Bearer token (use "Get a token")' });
    tokenIn.addEventListener('input', () => { st.token = tokenIn.value.trim(); });
    const connectBtn = h('button', { class: 'btn primary', type: 'button' }, icon('link'), 'Connect');
    const disconnectBtn = h('button', { class: 'btn', type: 'button', disabled: true }, 'Disconnect');

    endpointSel.addEventListener('change', () => { st.endpoint = endpointSel.value; renderAuth(); });

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'MCP inspector'),
        h('p', null, 'An MCP client in the browser. Connect directly or through the gateway to show MCP routing, auth, rate limits and tool filtering; the log shows every JSON-RPC message and the HTTP headers that carried it.'))));
    root.appendChild(h('div', { class: 'stack' },
      card('Connection', { actions: target.el }, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('div', { style: { minWidth: '260px' } }, endpointSel), versionSeg.el, connectBtn, disconnectBtn, statusEl),
        authEl)),
      pendingEl,
      h('div', { class: 'grid grid-2' }, mainEl,
        card('JSON-RPC log', { flush: true, actions: h('button', { class: 'btn sm', type: 'button', onclick: () => { st.log = []; renderLog(); } }, icon('trash'), 'Clear') }, logEl))));

    function renderAuth() {
      clear(authEl);
      if (st.endpoint === '/mcp/apikey') {
        authEl.appendChild(h('div', { class: 'grid grid-3' }, field('X-API-Key', apiKeyIn, 'Any non-empty key unless RUSTYBIN_MCP_API_KEY is set')));
      } else if (st.endpoint === '/mcp/protected') {
        const getBtn = h('button', { class: 'btn', type: 'button', onclick: discoverAndGetToken }, icon('token'), 'Get a token');
        authEl.appendChild(h('div', { class: 'stack-sm' },
          h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, field('Bearer token', tokenIn, 'From the built-in IdP (client_credentials, rustybin / secret, RFC 8707 resource)')), getBtn),
          stepsEl));
      }
    }
    const stepsEl = h('div', { class: 'stack-sm' });

    // ── Logging ──
    function log(dir, json, extra) {
      st.log.push({ dir, json, t: new Date().toISOString(), extra: extra || {} });
      if (st.log.length > 300) st.log.splice(0, st.log.length - 300);
      renderLog();
    }
    let logPending = false;
    function renderLog() {
      if (logPending) return;
      logPending = true;
      requestAnimationFrame(() => {
        logPending = false;
        if (!alive) return;
        clear(logEl);
        if (!st.log.length) { logEl.appendChild(empty('No messages yet', 'Connect to see the handshake.')); return; }
        for (const e of st.log.slice().reverse()) {
          const label = e.json.method || (e.json.result !== undefined ? 'result' : e.json.error ? 'error' : e.extra.note || 'http');
          const row = h('div', { class: 'log-row', tabindex: '0', role: 'button', 'aria-expanded': 'false' },
            h('span', { class: 'muted' }, fmtTime(e.t)),
            h('span', { class: e.dir === 'out' ? 'dir-out' : e.dir === 'in' ? 'dir-in' : 'dir-note' }, e.dir === 'out' ? 'SEND' : e.dir === 'in' ? 'RECV' : 'NOTE'),
            h('span', { class: 'ellipsis' }, label, e.json.id !== undefined ? h('span', { class: 'muted' }, ' #' + e.json.id) : null,
              e.json.params && e.json.params.name ? h('span', { class: 'muted' }, ' ' + e.json.params.name) : null,
              e.json.error ? h('span', { class: 'st st-5' }, ' ' + (e.json.error.code ?? '')) : null));
          let open = false;
          const toggle = () => {
            open = !open;
            row.setAttribute('aria-expanded', String(open));
            const existing = row.querySelector('.log-detail');
            if (existing) existing.remove();
            if (open) {
              const d = h('div', { class: 'log-detail', style: { gridColumn: '1 / -1' } }, jsonView(e.json, 'short'));
              if (e.extra.headers) d.appendChild(h('details', { class: 'disclosure' }, h('summary', null, 'HTTP request headers'), headersTable(e.extra.headers, { noHighlight: true })));
              row.appendChild(d);
            }
          };
          row.addEventListener('click', (ev) => { if (!ev.target.closest('.log-detail')) toggle(); });
          row.addEventListener('keydown', (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); toggle(); } });
          logEl.appendChild(row);
        }
      });
    }

    // ── Server to client requests (elicitation, sampling) ──
    function askUser(m) {
      return new Promise((resolve) => {
        const method = m.method;
        if (method === 'elicitation/create') {
          const p = m.params || {};
          const form = schemaForm(p.requestedSchema || { type: 'object', properties: {} });
          const err = h('div');
          const done = (action) => {
            let content;
            if (action === 'accept') {
              try { content = form.value(); } catch (e) { replace(err, notice('err', e.message)); return; }
            }
            clear(pendingEl);
            resolve({ result: action === 'accept' ? { action, content } : { action } });
          };
          replace(pendingEl, card('The server asks for input (elicitation)', { class: 'dialog-like' }, h('div', { class: 'stack-sm' },
            h('p', null, p.message || 'Input requested'), form.el, err,
            h('div', { class: 'row' }, h('button', { class: 'btn primary', type: 'button', onclick: () => done('accept') }, 'Accept'),
              h('button', { class: 'btn', type: 'button', onclick: () => done('decline') }, 'Decline'),
              h('button', { class: 'btn ghost', type: 'button', onclick: () => done('cancel') }, 'Cancel')))));
        } else if (method === 'sampling/createMessage') {
          const p = m.params || {};
          const reply = input({ value: 'This is a reply written in the Rustybin console (acting as the LLM).' });
          const msgs = (p.messages || []).map((x) => `${x.role}: ${x.content && x.content.text ? x.content.text : JSON.stringify(x.content)}`).join('\n');
          replace(pendingEl, card('The server asks the client to sample an LLM', { class: 'dialog-like' }, h('div', { class: 'stack-sm' },
            codeBlock(msgs || JSON.stringify(p, null, 2), null, 'short'), field('Reply', reply),
            h('div', { class: 'row' }, h('button', { class: 'btn primary', type: 'button', onclick: () => { clear(pendingEl); resolve({ result: { role: 'assistant', content: { type: 'text', text: reply.value }, model: 'rustybin-console', stopReason: 'endTurn' } }); } }, 'Send reply'),
              h('button', { class: 'btn', type: 'button', onclick: () => { clear(pendingEl); resolve({ error: { code: -1, message: 'User rejected sampling request' } }); } }, 'Reject')))));
        } else {
          resolve({ error: { code: -32601, message: 'Method not supported by the console: ' + method } });
        }
      });
    }

    // ── Connect ──
    function authHeaders() {
      if (st.endpoint === '/mcp/apikey' && st.apiKey) return [['X-API-Key', st.apiKey]];
      if (st.endpoint === '/mcp/protected' && st.token) return [['Authorization', 'Bearer ' + st.token]];
      return [];
    }

    async function connect() {
      if (client) await disconnect();
      connectBtn.disabled = true;
      replace(statusEl, spinner('connecting'));
      replace(mainEl, card(null, null, spinner('Connecting')));
      client = new McpClient({ url: target.base() + st.endpoint, version: st.version, authHeaders: authHeaders(), log, onServerRequest: askUser });
      const res = await client.connect();
      connectBtn.disabled = false;
      if (!alive) return;
      if (res.error || !res.result) {
        const status = res.status;
        replace(statusEl, statusBadge(status || 0, 'failed'));
        const www = res.headers && (res.headers.find(([k]) => k === 'www-authenticate') || [])[1];
        replace(mainEl, card('Connection failed', null, h('div', { class: 'stack-sm' },
          notice('err', `${res.error.message || 'error'}${res.error.code ? ' (code ' + res.error.code + ')' : ''}`),
          status === 401 ? notice('warn', 'The server requires a bearer token. ', www ? h('code', null, 'WWW-Authenticate: ' + www) : null, ' Use "Get a token" to run the OAuth discovery.') : null,
          res.error.data ? jsonView(res.error.data, 'short') : null)));
        client = null;
        if (status === 401 && st.endpoint !== '/mcp/protected') toast('401 from the server', 'err');
        return;
      }
      server = res.result;
      disconnectBtn.disabled = false;
      const info = (server._meta && server._meta['io.modelcontextprotocol/serverInfo']) || server.serverInfo || {};
      replace(statusEl, badge('connected', 'ok'), h('span', { class: 'small muted' }, `${info.title || info.name || 'server'} ${info.version || ''}`),
        client.sessionId ? h('span', { class: 'small mono muted', title: 'Mcp-Session-Id' }, 'session ' + client.sessionId.slice(0, 12)) : h('span', { class: 'small muted' }, 'stateless'));
      renderMain();
    }

    async function disconnect() {
      if (callCtrl) callCtrl.abort();
      if (client) await client.close();
      client = null;
      server = null;
      disconnectBtn.disabled = true;
      replace(statusEl, badge('not connected'));
      replace(mainEl, card(null, null, empty('Not connected', 'Pick an endpoint and protocol version, then Connect.', 'mcp')));
    }

    connectBtn.addEventListener('click', connect);
    disconnectBtn.addEventListener('click', disconnect);

    // ── Tools / resources / prompts ──
    let mainTab = settings.recall('mcp.tab', 'tools');
    function renderMain() {
      const caps = server.capabilities || {};
      const t = tabs([
        { id: 'tools', label: 'Tools', render: toolsPane },
        { id: 'resources', label: 'Resources', render: resourcesPane },
        { id: 'prompts', label: 'Prompts', render: promptsPane },
        { id: 'server', label: 'Server', render: () => h('div', { class: 'card-body stack-sm' },
          server.instructions ? h('p', { class: 'small' }, server.instructions) : null,
          kvgrid([['Capabilities', Object.keys(caps).join(', ') || 'none'], ['Versions', (server.supportedVersions || [server.protocolVersion]).filter(Boolean).join(', ')]]),
          jsonView(server, 'tall')) },
      ], mainTab, (id) => { mainTab = id; settings.remember('mcp.tab', id); });
      replace(mainEl, h('div', { class: 'card' }, t.el));
    }

    function listPane(method, key, renderItem) {
      const host = h('div', { class: 'card-body' }, spinner(method));
      (async () => {
        const items = [];
        let cursor;
        for (let page = 0; page < 10; page++) {
          const res = await client.request(method, cursor ? { cursor } : {});
          if (!alive || !client) return;
          if (res.error) { replace(host, notice('err', `${method} failed: ${res.error.message}`)); return; }
          items.push(...(res.result[key] || []));
          cursor = res.result.nextCursor;
          if (!cursor) break;
        }
        renderItem(host, items);
      })();
      return host;
    }

    function toolsPane() {
      return listPane('tools/list', 'tools', (host, tools) => {
        for (const t of tools) client.toolSchemas.set(t.name, t.inputSchema);
        const detail = h('div');
        const list = h('div', { class: 'pill-list' }, ...tools.map((t) => h('button', { class: 'btn sm', type: 'button', onclick: () => pick(t) }, t.name)));
        function pick(t) {
          for (const b of list.children) b.classList.toggle('active', b.textContent === t.name);
          renderTool(detail, t);
        }
        replace(host, h('div', { class: 'stack' }, h('div', { class: 'small muted' }, `${tools.length} tools`), list, detail));
        const first = tools.find((t) => t.name === 'slow_task') || tools[0];
        if (first) pick(first);
      });
    }

    function renderTool(host, t) {
      const form = schemaForm(t.inputSchema, t.name === 'slow_task' ? { duration_ms: 3000, steps: 5 } : t.name === 'get_weather' ? { city: 'Paris' } : null);
      const result = h('div');
      const callBtn = h('button', { class: 'btn primary', type: 'button' }, icon('play'), 'Call tool');
      const cancelBtn = h('button', { class: 'btn', type: 'button', hidden: true }, icon('stop'), 'Cancel');
      callBtn.addEventListener('click', () => callTool(t, form, result, callBtn, cancelBtn));
      cancelBtn.addEventListener('click', () => { if (callCtrl) callCtrl.abort(); });
      const ann = t.annotations || {};
      replace(host, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('h3', { class: 'mono' }, t.name), ann.destructiveHint ? badge('destructive', 'err') : null, ann.readOnlyHint ? badge('read only', 'ok') : null, t.title ? h('span', { class: 'muted small' }, t.title) : null),
        t.description ? h('p', { class: 'small muted' }, t.description) : null,
        form.el, h('div', { class: 'row' }, callBtn, cancelBtn), result));
    }

    async function callTool(t, form, result, callBtn, cancelBtn) {
      let args;
      try { args = form.value(); } catch (e) { replace(result, notice('err', e.message)); return; }
      callCtrl = new AbortController();
      const bar = h('div');
      const progress = h('div', { class: 'progress', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100' }, bar);
      const progressText = h('span', { class: 'small muted' }, 'calling...');
      const notes = h('div', { class: 'stack-sm' });
      replace(result, h('div', { class: 'stack-sm' }, h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, progress), progressText), notes));
      callBtn.disabled = true;
      cancelBtn.hidden = false;
      const started = performance.now();
      const token = 'console-' + Date.now();
      let params = { name: t.name, arguments: args };
      let res;
      for (let round = 0; round < 4; round++) {
        res = await client.request('tools/call', params, {
          progressToken: token, signal: callCtrl.signal,
          onNotification: (m) => {
            if (m.method === 'notifications/progress') {
              const p = m.params || {};
              const pct = p.total ? Math.min(100, (p.progress / p.total) * 100) : 50;
              bar.style.width = pct + '%';
              progress.setAttribute('aria-valuenow', String(Math.round(pct)));
              progressText.textContent = `${p.message || 'progress'} (${p.progress}${p.total ? '/' + p.total : ''})`;
            } else if (m.method === 'notifications/message') {
              notes.appendChild(h('div', { class: 'small mono muted' }, `[${(m.params && m.params.level) || 'log'}] ${JSON.stringify(m.params && m.params.data)}`));
            }
          },
        });
        // 2026-07-28 multi round-trip: answer input requests, then retry.
        if (res.result && res.result.resultType === 'input_required') {
          const responses = {};
          for (const [key, req] of Object.entries(res.result.inputRequests || {})) {
            const answer = await askUser({ method: req.method, params: req.params });
            responses[key] = answer.result || { action: 'decline' };
          }
          params = { name: t.name, arguments: args, inputResponses: responses };
          if (res.result.requestState) params.requestState = res.result.requestState;
          continue;
        }
        break;
      }
      callBtn.disabled = false;
      cancelBtn.hidden = true;
      callCtrl = null;
      if (!alive) return;
      const ms = performance.now() - started;
      if (res.error) {
        bar.style.width = '100%';
        bar.style.background = 'var(--err)';
        progressText.textContent = fmtMs(ms);
        result.appendChild(notice('err', `${res.error.message}${res.error.code !== undefined ? ' (code ' + res.error.code + ')' : ''}`, res.status === 403 ? ' The token lacks a scope for this tool (insufficient_scope).' : ''));
        if (res.error.data) result.appendChild(jsonView(res.error.data, 'short'));
        return;
      }
      bar.style.width = '100%';
      progressText.textContent = 'done in ' + fmtMs(ms);
      result.appendChild(contentView(res.result));
    }

    function contentView(r) {
      const out = h('div', { class: 'stack-sm' });
      if (r.isError) out.appendChild(notice('err', 'The tool reported an error (isError: true).'));
      for (const c of r.content || []) {
        if (c.type === 'text') out.appendChild(codeBlock(c.text, /^\s*[{[]/.test(c.text) ? 'json' : null, 'short'));
        else if (c.type === 'image') out.appendChild(h('div', { class: 'artifact' }, badge('image ' + c.mimeType), h('img', { src: `data:${c.mimeType};base64,${c.data}`, alt: 'tool image output' })));
        else if (c.type === 'resource_link') out.appendChild(h('div', { class: 'artifact' }, badge('resource link', 'info'), ' ', h('code', null, c.uri), c.description ? h('div', { class: 'small muted' }, c.description) : null));
        else if (c.type === 'resource') out.appendChild(h('div', { class: 'artifact' }, badge('embedded resource', 'info'), jsonView(c.resource, 'short')));
        else out.appendChild(jsonView(c, 'short'));
      }
      if (r.structuredContent) out.appendChild(h('details', { class: 'disclosure', open: true }, h('summary', null, 'structuredContent'), jsonView(r.structuredContent, 'short')));
      return out;
    }

    function resourcesPane() {
      return listPane('resources/list', 'resources', (host, resources) => {
        const detail = h('div');
        replace(host, h('div', { class: 'stack' },
          h('div', { class: 'list', style: { border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)' } }, ...resources.map((r) =>
            h('button', { class: 'list-item', type: 'button', style: { gridTemplateColumns: 'minmax(0,1fr) auto' }, onclick: () => read(r, detail) },
              h('span', null, h('div', { class: 'mono small' }, r.uri), h('div', { class: 'tiny muted' }, r.description || r.title || r.name)), badge(r.mimeType || '', 'outline')))),
          detail));
        if (!resources.length) replace(host, empty('No resources'));
      });
    }

    async function read(r, detail) {
      replace(detail, spinner('resources/read'));
      const res = await client.request('resources/read', { uri: r.uri });
      if (!alive) return;
      if (res.error) { replace(detail, notice('err', res.error.message)); return; }
      replace(detail, h('div', { class: 'stack-sm' }, ...(res.result.contents || []).map((c) =>
        c.text !== undefined ? h('div', null, badge(c.mimeType || 'text', 'outline'), codeBlock(c.text, (c.mimeType || '').includes('json') ? 'json' : null, 'tall'))
          : c.blob && (c.mimeType || '').startsWith('image/') ? h('div', { class: 'artifact' }, badge(c.mimeType), h('img', { src: `data:${c.mimeType};base64,${c.blob}`, alt: c.uri }))
            : jsonView(c, 'short'))));
    }

    function promptsPane() {
      return listPane('prompts/list', 'prompts', (host, prompts) => {
        const detail = h('div');
        const list = h('div', { class: 'pill-list' }, ...prompts.map((p) => h('button', { class: 'btn sm', type: 'button', onclick: () => renderPrompt(p, detail) }, p.name)));
        replace(host, h('div', { class: 'stack' }, list, detail));
        if (prompts[0]) renderPrompt(prompts[0], detail);
      });
    }

    function renderPrompt(p, detail) {
      const form = schemaForm(promptArgsSchema(p.arguments));
      const out = h('div');
      const btn = h('button', { class: 'btn primary', type: 'button' }, 'Get prompt');
      btn.addEventListener('click', async () => {
        let args;
        try { args = form.value(); } catch (e) { replace(out, notice('err', e.message)); return; }
        replace(out, spinner('prompts/get'));
        const res = await client.request('prompts/get', { name: p.name, arguments: args });
        if (!alive) return;
        if (res.error) { replace(out, notice('err', res.error.message)); return; }
        replace(out, h('div', { class: 'stack-sm' }, res.result.description ? h('p', { class: 'small muted' }, res.result.description) : null,
          ...(res.result.messages || []).map((m) => h('div', { class: 'msg ' + (m.role === 'user' ? 'user' : 'assistant'), style: { maxWidth: '100%' } },
            m.content && m.content.type === 'text' ? m.content.text : JSON.stringify(m.content, null, 2)))));
      });
      replace(detail, h('div', { class: 'stack-sm' }, h('h3', { class: 'mono' }, p.name), p.description ? h('p', { class: 'small muted' }, p.description) : null, form.el, h('div', null, btn), out));
    }

    // ── OAuth discovery for /mcp/protected ──
    async function discoverAndGetToken() {
      clear(stepsEl);
      const base = target.base();
      const step = (n, title, body) => {
        const el = h('div', { class: 'step' }, h('span', { class: 'step-n' }, String(n)), h('div', { class: 'stack-sm' }, h('strong', null, title), body));
        stepsEl.appendChild(el);
        return el;
      };
      // 1. Unauthenticated request.
      const probe = new McpClient({ url: base + '/mcp/protected', version: MODERN, log });
      const s1 = h('div', null, spinner('POST /mcp/protected without a token'));
      step(1, 'Call the server without a token', s1);
      const first = await probe.request('server/discover', {});
      const www = (first.headers.find(([k]) => k === 'www-authenticate') || [])[1] || '';
      replace(s1, h('div', { class: 'stack-sm' }, h('div', { class: 'row' }, statusBadge(first.status), h('span', { class: 'small' }, 'The server answers 401 with a challenge:')), codeBlock('WWW-Authenticate: ' + (www || '(missing: the gateway may not expose it via CORS)'), null, 'short')));
      if (first.status !== 401) { stepsEl.appendChild(notice('warn', 'Expected 401. Is a gateway already authenticating this route?')); }
      const ch = parseChallenge(www);
      const prmUrl = ch.resource_metadata || base + '/.well-known/oauth-protected-resource/mcp/protected';
      // 2. Protected resource metadata.
      const s2 = h('div', null, spinner('GET ' + prmUrl));
      step(2, 'Fetch the protected resource metadata (RFC 9728)', s2);
      const prm = await send({ url: prmUrl, timeout: 10000 });
      if (!prm.json) { replace(s2, notice('err', prm.error || `answered ${prm.status}`)); return; }
      replace(s2, h('div', { class: 'stack-sm' }, h('code', { class: 'small break' }, 'GET ' + prmUrl), jsonView(prm.json, 'short')));
      // 3. Authorization server metadata.
      const as = (prm.json.authorization_servers || [base])[0].replace(/\/$/, '');
      const asUrl = as + '/.well-known/oauth-authorization-server';
      const s3 = h('div', null, spinner('GET ' + asUrl));
      step(3, 'Discover the authorization server (RFC 8414)', s3);
      const asm = await send({ url: asUrl, timeout: 10000 });
      if (!asm.json) { replace(s3, notice('err', asm.error || `answered ${asm.status}`)); return; }
      replace(s3, h('div', { class: 'stack-sm' }, kvgrid([['issuer', asm.json.issuer], ['token_endpoint', asm.json.token_endpoint], ['grants', (asm.json.grant_types_supported || []).join(', ')]])));
      // 4. Token request.
      const form = new URLSearchParams({ grant_type: 'client_credentials', client_id: 'rustybin', client_secret: 'secret', resource: prm.json.resource, scope: (prm.json.scopes_supported || ['mcp:tools']).join(' ') });
      const s4 = h('div', null, spinner('POST ' + asm.json.token_endpoint));
      step(4, 'Get a token for this resource (client_credentials, RFC 8707 resource)', s4);
      const tok = await send({ method: 'POST', url: asm.json.token_endpoint, headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: form.toString(), timeout: 10000 });
      if (!tok.json || !tok.json.access_token) { replace(s4, notice('err', tok.error || (tok.json && (tok.json.error_description || tok.json.error)) || `answered ${tok.status}`)); return; }
      const claims = decodeJwt(tok.json.access_token);
      replace(s4, h('div', { class: 'stack-sm' }, codeBlock(toCurl({ method: 'POST', url: asm.json.token_endpoint, headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: form.toString() }), null, 'short'),
        claims ? kvgrid([['aud', JSON.stringify(claims.payload.aud)], ['scope', claims.payload.scope], ['expires', new Date(claims.payload.exp * 1000).toLocaleTimeString()]]) : null));
      st.token = tok.json.access_token;
      tokenIn.value = st.token;
      step(5, 'Connect with Authorization: Bearer', h('div', { class: 'row' }, h('span', { class: 'small muted' }, 'Token stored above.'), h('button', { class: 'btn primary sm', type: 'button', onclick: connect }, 'Connect now')));
    }

    renderAuth();
    renderLog();
    replace(mainEl, card(null, null, empty('Not connected', 'Pick an endpoint and protocol version, then Connect.', 'mcp')));
    return () => { alive = false; if (callCtrl) callCtrl.abort(); if (client) client.close(); if (target.destroy) target.destroy(); };
  },
};
