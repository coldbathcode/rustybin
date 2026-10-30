// MCP inspector: connect to any MCP server (this instance's servers from
// GET /_rustybin/mcp-servers, saved connections, or any URL), over every
// transport (2026-07-28 stateless, 2025 sessions, legacy HTTP+SSE), in every
// auth mode (none, bearer, API key, basic, custom headers, OAuth 2.1 with
// discovery, registration and PKCE), directly from the browser or through
// the instance's relay. Three columns: the connection on the left, the
// protocol (tools, resources, prompts, server, checks) in the middle, the
// timeline of every JSON-RPC message on the right, with exports.
import { h, replace, clear, card, field, input, select, notice, empty, icon, badge, statusBadge, fmtMs, fmtTime, segmented, tabs, spinner, toast, kvgrid, checkbox, textarea, copyButton } from '../lib/dom.js';
import { jsonView, codeBlock, headersTable } from '../lib/format.js';
import { SERVER, send, getJson, maskCredential } from '../lib/http.js';
import { schemaForm, promptArgsSchema } from '../lib/schemaform.js';
import { McpClient, VERSIONS, MODERN, transportOf, exportSession } from '../lib/mcpclient.js';
import { MODES, buildAuth, withQuery, oauthWalk, refreshToken } from '../lib/mcpauth.js';
import * as settings from '../lib/settings.js';
import * as auth from '../lib/auth.js';
import * as assist from '../lib/assist.js';

const SOURCES = [['instance', 'This instance'], ['saved', 'Saved'], ['url', 'Another server']];

function defaultState() {
  return {
    source: 'instance', server: '/mcp', url: '', saved: '', version: MODERN, mode: 'browser',
    auth: { kind: 'none', token: '', name: 'X-API-Key', location: 'header', key: '', username: '', password: '', headers: [['', '']], grant: 'authorization_code', clientId: '', clientSecret: '', scope: '' },
    log: [],
  };
}

export default {
  id: 'mcp',
  mount(root, { rest }) {
    let alive = true;
    const st = settings.recall('mcp.v2', null) || defaultState();
    settings.remember('mcp.v2', st);
    let servers = [];
    let checksList = [];
    let relayInfo = null;
    let saved = [];
    let client = null;
    let server = null;
    let callCtrl = null;
    let oauthState = null;

    // ── Columns ──
    const leftEl = h('div', { class: 'stack mcp-left' });
    const mainEl = h('div', { class: 'mcp-main' });
    const logEl = h('div', { class: 'log', id: 'mcp-log' });
    const pendingEl = h('div');
    const statusEl = h('div', { class: 'stack-sm', id: 'mcp-status' }, badge('not connected'));
    const exportEl = h('div');
    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'MCP inspector'),
        h('p', null, 'An MCP client for any server: this instance\'s, a saved connection or any URL, in every transport and auth mode, from the browser or through the instance\'s relay. The timeline shows every JSON-RPC message and the headers that carried it; Checks runs a repeatable test suite with a JUnit report.'))));
    root.appendChild(h('div', { class: 'mcp-layout' },
      leftEl,
      h('div', { class: 'stack', style: { minWidth: 0 } }, pendingEl, mainEl),
      h('div', { class: 'stack mcp-right' },
        card('Timeline', { flush: true, actions: h('button', { class: 'btn sm', type: 'button', onclick: () => { st.log = []; renderLog(); } }, icon('trash'), 'Clear') }, logEl),
        card('Export this session', { hint: 'Hurl, curl, Python MCP SDK, claude mcp add' }, exportEl))));

    // ── The connection column ──
    const sourceSeg = segmented(SOURCES, st.source, (v) => { st.source = v; renderLeft(); }, 'Server source');
    const serverSel = h('select', { class: 'select', 'aria-label': 'Server of this instance', id: 'mcp-server' });
    const urlIn = input({ value: st.url, placeholder: 'https://mcp.example.com/mcp', 'aria-label': 'Server URL', id: 'mcp-url', class: 'input mono' });
    urlIn.addEventListener('input', () => { st.url = urlIn.value.trim(); });
    const savedSel = h('select', { class: 'select', 'aria-label': 'Saved connection', id: 'mcp-saved' });
    const versionSel = select(VERSIONS.map(([v, label]) => [v, label]), st.version, { 'aria-label': 'Protocol version', id: 'mcp-version' });
    versionSel.addEventListener('change', () => { st.version = versionSel.value; });
    const modeSeg = segmented([['browser', 'Browser'], ['relay', 'Relay']], st.mode, (v) => { st.mode = v; }, 'Connection mode');
    const authEl = h('div', { class: 'stack-sm' });
    const connectBtn = h('button', { class: 'btn primary', type: 'button', id: 'mcp-connect' }, icon('link'), 'Connect');
    const disconnectBtn = h('button', { class: 'btn', type: 'button', disabled: true, id: 'mcp-disconnect' }, 'Disconnect');
    connectBtn.addEventListener('click', () => connect());
    disconnectBtn.addEventListener('click', () => disconnect());

    function targetPath() {
      if (st.source === 'url') return st.url;
      if (st.source === 'saved') { const c = saved.find((x) => x.id === st.saved); return c ? c.target : ''; }
      return st.server;
    }

    function serverInfo() { return servers.find((s) => s.path === st.server) || null; }

    function applyServerDefaults(s) {
      if (!s) return;
      const a = s.auth || {};
      st.auth.kind = a.kind === 'spec' ? 'bearer' : a.kind || 'none';
      if (a.kind === 'api_key') { st.auth.name = a.name || 'X-API-Key'; st.auth.location = 'header'; if (!st.auth.key) st.auth.key = 'demo-key'; }
      if (a.kind === 'oauth') { st.auth.grant = 'client_credentials'; st.auth.clientId = st.auth.clientId || 'rustybin'; st.auth.clientSecret = st.auth.clientSecret || 'secret'; }
      if (s.kind === 'manage' && auth.required() && auth.getToken()) st.auth.token = auth.getToken();
      const v = (s.versions || [])[0];
      if ((s.transports || []).length === 1 && s.transports[0] === 'legacy') st.version = '2024-11-05';
      else if (transportOf(st.version) === 'legacy') st.version = v || MODERN;
      versionSel.value = st.version;
    }

    function renderLeft() {
      clear(leftEl);
      clear(serverSel);
      for (const s of servers) {
        const o = h('option', { value: s.path }, s.title + ' (' + s.path + ')' + (s.state !== 'serving' ? ' [' + s.state + ']' : ''));
        if (s.path === st.server) o.selected = true;
        serverSel.appendChild(o);
      }
      serverSel.onchange = () => { st.server = serverSel.value; applyServerDefaults(serverInfo()); renderLeft(); };
      clear(savedSel);
      savedSel.appendChild(h('option', { value: '' }, saved.length ? 'Choose a saved connection' : 'No saved connections yet'));
      for (const c of saved) {
        const o = h('option', { value: c.id }, c.name + ' (' + c.target + ')');
        if (c.id === st.saved) o.selected = true;
        savedSel.appendChild(o);
      }
      savedSel.onchange = () => {
        st.saved = savedSel.value;
        const c = saved.find((x) => x.id === st.saved);
        if (c) {
          st.version = c.version || (c.transport === 'legacy' ? '2024-11-05' : c.transport === 'session' ? '2025-11-25' : MODERN);
          versionSel.value = st.version;
          // Saved credentials stay on the instance: a connection that holds any goes through the relay.
          st.mode = Object.keys(c.secrets || {}).length ? 'relay' : c.mode || 'browser';
          modeSeg.set(st.mode);
          st.auth.kind = (c.auth && c.auth.kind) || 'none';
        }
        renderLeft();
      };
      const s = serverInfo();
      const picker = st.source === 'instance'
        ? h('div', { class: 'stack-sm' }, serverSel,
          s ? h('div', { class: 'small muted' }, 'Transports ', (s.transports || []).join(', '), ' | auth ', (s.auth && s.auth.kind) || 'none', s.auth && s.auth.hint ? ': ' + s.auth.hint : '', s.tools !== undefined ? ' | ' + s.tools + ' tools' : '') : null)
        : st.source === 'saved'
          ? h('div', { class: 'stack-sm' }, savedSel, savedNote())
          : field('Server URL', urlIn, relayInfo ? 'Through the relay the host must be in the outbound allowlist (' + (relayInfo.allow.length ? relayInfo.allow.join(', ') : 'empty') + ').' : null);
      leftEl.appendChild(card('Server', { hint: 'what to connect to' }, h('div', { class: 'stack-sm' },
        sourceSeg.el, picker,
        h('div', { class: 'grid grid-2' }, field('Protocol version', versionSel), h('div', { class: 'field' }, h('span', null, 'Mode'), modeSeg.el,
          h('span', { class: 'hint' }, 'Relay: the instance connects for you (no CORS needed).'))),
        authEl,
        h('div', { class: 'row' }, connectBtn, disconnectBtn),
        statusEl)));
      renderAuth();
      leftEl.appendChild(saveCard());
    }

    function savedNote() {
      const c = saved.find((x) => x.id === st.saved);
      if (!c) return null;
      const secrets = Object.entries(c.secrets || {}).map(([k, v]) => k + (v.hint ? ' (ends in ' + v.hint + ')' : ''));
      return h('div', { class: 'small muted' }, 'Auth ', (c.auth && c.auth.kind) || 'none', secrets.length ? ', credentials kept on the instance: ' + secrets.join(', ') : '', '.');
    }

    // ── Auth ──
    const oauthSteps = h('div', { class: 'stack-sm', id: 'mcp-oauth-steps' });
    function renderAuth() {
      clear(authEl);
      const a = st.auth;
      if (st.source === 'saved' && st.saved) {
        authEl.appendChild(notice('', 'The saved connection\'s credentials are added by the relay.'));
        return;
      }
      const kindSel = select(MODES, a.kind, { 'aria-label': 'Authentication', id: 'mcp-auth-kind' });
      kindSel.addEventListener('change', () => { a.kind = kindSel.value; renderAuth(); });
      const bind = (key, props) => { const el = input(Object.assign({ value: a[key] || '' }, props)); el.addEventListener('input', () => { a[key] = el.value; }); return el; };
      authEl.appendChild(field('Authentication', kindSel));
      if (a.kind === 'bearer') authEl.appendChild(field('Bearer token', bind('token', { type: 'password', class: 'input mono', 'aria-label': 'Bearer token', id: 'mcp-token' })));
      if (a.kind === 'api_key') {
        const loc = select([['header', 'Header'], ['query', 'Query parameter']], a.location || 'header', { 'aria-label': 'API key location' });
        loc.addEventListener('change', () => { a.location = loc.value; });
        authEl.appendChild(h('div', { class: 'grid grid-2' }, field('Name', bind('name', { class: 'input mono', 'aria-label': 'API key name' })), field('In', loc)));
        authEl.appendChild(field('API key', bind('key', { type: 'password', class: 'input mono', 'aria-label': 'API key', id: 'mcp-api-key' })));
      }
      if (a.kind === 'basic') authEl.appendChild(h('div', { class: 'grid grid-2' }, field('User', bind('username', { 'aria-label': 'User' })), field('Password', bind('password', { type: 'password', 'aria-label': 'Password' }))));
      if (a.kind === 'headers') {
        const rows = h('div', { class: 'stack-sm' });
        const draw = () => {
          clear(rows);
          a.headers.forEach((pair, i) => {
            const k = input({ value: pair[0], placeholder: 'Header', class: 'input mono', 'aria-label': 'Header name ' + (i + 1) });
            const v = input({ value: pair[1], placeholder: 'Value', class: 'input mono', 'aria-label': 'Header value ' + (i + 1) });
            k.addEventListener('input', () => { pair[0] = k.value; });
            v.addEventListener('input', () => { pair[1] = v.value; });
            rows.appendChild(h('div', { class: 'row nowrap' }, k, v, h('button', { class: 'btn sm ghost icon', type: 'button', 'aria-label': 'Remove header', onclick: () => { a.headers.splice(i, 1); if (!a.headers.length) a.headers.push(['', '']); draw(); } }, icon('x'))));
          });
          rows.appendChild(h('button', { class: 'btn sm', type: 'button', onclick: () => { a.headers.push(['', '']); draw(); } }, icon('plus'), 'Add header'));
        };
        draw();
        authEl.appendChild(h('div', { class: 'field' }, h('span', null, 'Headers'), rows));
      }
      if (a.kind === 'oauth') {
        const grant = select([['authorization_code', 'Authorization code + PKCE (popup)'], ['client_credentials', 'Client credentials']], a.grant, { 'aria-label': 'OAuth grant', id: 'mcp-oauth-grant' });
        grant.addEventListener('change', () => { a.grant = grant.value; });
        const getBtn = h('button', { class: 'btn', type: 'button', id: 'mcp-oauth-start' }, icon('token'), 'Get a token');
        getBtn.addEventListener('click', () => runOAuth(getBtn));
        const refreshBtn = oauthState && oauthState.refresh ? h('button', { class: 'btn sm', type: 'button', onclick: doRefresh }, icon('refresh'), 'Refresh the token') : null;
        authEl.appendChild(h('div', { class: 'stack-sm' },
          field('Grant', grant),
          h('div', { class: 'grid grid-2' }, field('Client id', bind('clientId', { placeholder: 'empty: register dynamically', 'aria-label': 'Client id', id: 'mcp-oauth-client' })), field('Client secret', bind('clientSecret', { type: 'password', 'aria-label': 'Client secret' }))),
          field('Scope', bind('scope', { placeholder: 'empty: what the server advertises', 'aria-label': 'Scope' })),
          field('Access token', bind('token', { type: 'password', class: 'input mono', 'aria-label': 'Access token', id: 'mcp-token' }), 'Filled by Get a token, or paste one.'),
          h('div', { class: 'row' }, getBtn, refreshBtn),
          oauthSteps));
      }
    }

    function authParts() {
      if (st.source === 'saved' && st.saved) return { headers: [], query: [] };
      return buildAuth(st.auth);
    }

    // The OAuth walk as a timeline of steps with the raw documents.
    async function runOAuth(btn) {
      clear(oauthSteps);
      btn.disabled = true;
      const step = (n, title, detail) => {
        const body = h('div', { class: 'stack-sm' }, h('span', { class: 'small muted break' }, detail || ''), spinner());
        const el = h('div', { class: 'step' }, h('span', { class: 'step-n' }, String(n)), h('div', { class: 'stack-sm', style: { minWidth: 0 } }, h('strong', null, title), body));
        oauthSteps.appendChild(el);
        return {
          done(doc, via) { replace(body, h('span', { class: 'small muted break' }, detail || ''), via ? badge('via ' + via, 'outline') : null, doc ? h('details', { class: 'disclosure' }, h('summary', null, 'Document'), jsonView(doc, 'short')) : null); },
          fail(msg) { replace(body, h('span', { class: 'small muted break' }, detail || ''), notice('err', msg)); },
        };
      };
      try {
        // The probe: an unauthenticated call for the challenge.
        const probe = new McpClient({ url: clientUrl([]), mode: st.mode, version: st.version, log });
        const first = await probe.connect();
        const www = (first.headers.find(([k]) => String(k).toLowerCase() === 'www-authenticate') || [])[1] || '';
        if (first.status !== 401) oauthSteps.appendChild(notice('warn', 'Expected a 401 from the server, got ' + (first.status || 'no answer') + '. Is a gateway already authenticating this route?'));
        oauthState = await oauthWalk({ resource: absoluteTarget(), challenge: www, mode: st.mode, grant: st.auth.grant, clientId: st.auth.clientId, clientSecret: st.auth.clientSecret, scope: st.auth.scope, step });
        st.auth.token = oauthState.token;
        if (oauthState.clientId) st.auth.clientId = oauthState.clientId;
        renderAuth();
        oauthSteps.appendChild(h('div', { class: 'row' }, h('span', { class: 'small muted' }, 'Token in place.'), h('button', { class: 'btn primary sm', type: 'button', id: 'mcp-oauth-connect', onclick: () => connect() }, 'Connect now')));
      } catch (e) {
        oauthSteps.appendChild(notice('err', 'The OAuth walk stopped: ' + (e.message || e)));
      } finally {
        btn.disabled = false;
      }
    }

    async function doRefresh() {
      try {
        const t = await refreshToken(oauthState);
        st.auth.token = t.access_token;
        if (t.refresh_token) oauthState.refresh = t.refresh_token;
        renderAuth();
        toast('Token refreshed');
      } catch (e) {
        toast('Refresh failed: ' + (e.message || e), 'err');
      }
    }

    function absoluteTarget() {
      const t = targetPath();
      return /^https?:\/\//i.test(t) ? t : SERVER + t;
    }

    function clientUrl(query) {
      const t = targetPath();
      return withQuery(t, query);
    }

    // ── Saving a connection ──
    function saveCard() {
      const idIn = input({ placeholder: 'id, e.g. staging-gateway', 'aria-label': 'Connection id', id: 'mcp-save-id', class: 'input mono' });
      const nameIn = input({ placeholder: 'name', 'aria-label': 'Connection name', id: 'mcp-save-name' });
      const msg = h('div');
      const btn = h('button', { class: 'btn sm', type: 'button', id: 'mcp-save' }, icon('check'), 'Save connection');
      btn.addEventListener('click', async () => {
        const id = idIn.value.trim();
        if (!/^[A-Za-z0-9_-]{1,64}$/.test(id)) { replace(msg, notice('err', 'The id: 1 to 64 letters, digits, - and _.')); return; }
        const a = st.auth;
        const secrets = {};
        const spec = { kind: a.kind };
        if (a.kind === 'bearer' || a.kind === 'oauth') { if (a.token) secrets.token = a.token; }
        if (a.kind === 'api_key') { spec.name = a.name || 'X-API-Key'; spec.location = a.location || 'header'; if (a.key) secrets.api_key = a.key; }
        if (a.kind === 'basic') { spec.username = a.username; if (a.password) secrets.password = a.password; }
        if (a.kind === 'headers') { spec.headers = a.headers.filter(([k]) => k).map(([k]) => k); for (const [k, v] of a.headers) if (k && v) secrets['header:' + k] = v; }
        if (a.kind === 'oauth') { spec.oauth = { client_id: a.clientId || null, scope: a.scope || null, token_endpoint: oauthState ? oauthState.tokenEndpoint : null, issuer: oauthState ? oauthState.issuer : null }; if (oauthState && oauthState.refresh) secrets.refresh_token = oauthState.refresh; if (a.clientSecret) secrets.client_secret = a.clientSecret; }
        const body = { name: nameIn.value.trim() || id, target: targetPath(), transport: transportOf(st.version), version: transportOf(st.version) === 'session' ? st.version : undefined, mode: st.mode, auth: spec, secrets };
        const r = await send({ method: 'PUT', url: SERVER + '/_rustybin/mcp-connections/' + encodeURIComponent(id), headers: [['Content-Type', 'application/json']], body: JSON.stringify(body), admin: true, session: false });
        if (!r.ok) { replace(msg, notice('err', (r.json && r.json.error) || r.error || ('HTTP ' + r.status))); return; }
        replace(msg);
        toast('Connection saved: credentials are sealed on the instance');
        await loadSaved();
        st.source = 'saved';
        st.saved = id;
        sourceSeg.set('saved');
        renderLeft();
      });
      return card('Save this connection', { hint: 'admin; credentials sealed on the instance' }, h('div', { class: 'stack-sm' },
        h('div', { class: 'grid grid-2' }, idIn, nameIn), h('div', { class: 'row' }, btn), msg));
    }

    // ── The timeline ──
    function log(dir, json, extra) {
      st.log.push({ dir, json, t: new Date().toISOString(), extra: extra || {} });
      if (st.log.length > 400) st.log.splice(0, st.log.length - 400);
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
        if (!st.log.length) logEl.appendChild(empty('No messages yet', 'Connect to see the handshake.'));
        for (const e of st.log.slice().reverse()) {
          const label = e.json.method || (e.json.result !== undefined ? 'result' : e.json.error ? 'error' : e.extra.note || e.json.action || 'http');
          const row = h('div', { class: 'log-row', tabindex: '0', role: 'button', 'aria-expanded': 'false' },
            h('span', { class: 'muted' }, fmtTime(e.t)),
            h('span', { class: e.dir === 'out' ? 'dir-out' : e.dir === 'in' ? 'dir-in' : 'dir-note' }, e.dir === 'out' ? 'SEND' : e.dir === 'in' ? 'RECV' : 'NOTE'),
            h('span', { class: 'ellipsis' }, label, e.json.id !== undefined ? h('span', { class: 'muted' }, ' #' + e.json.id) : null,
              e.json.params && e.json.params.name ? h('span', { class: 'muted' }, ' ' + e.json.params.name) : null,
              e.extra.mode === 'relay' ? h('span', { class: 'muted' }, ' (relay)') : null,
              e.extra.ms !== undefined ? h('span', { class: 'muted' }, ' ' + e.extra.ms + ' ms') : null,
              e.json.error ? h('span', { class: 'st st-5' }, ' ' + (e.json.error.code ?? '')) : null));
          let open = false;
          const toggle = () => {
            open = !open;
            row.setAttribute('aria-expanded', String(open));
            const existing = row.querySelector('.log-detail');
            if (existing) existing.remove();
            if (open) {
              const d = h('div', { class: 'log-detail', style: { gridColumn: '1 / -1' } }, jsonView(e.json, 'short'));
              if (e.extra.headers) {
                const shown = e.extra.headers.map(([k, v]) => [k, /^(authorization|proxy-authorization|cookie|x-api-key|api-key)$/i.test(k) ? maskCredential(v) : v]);
                d.appendChild(h('details', { class: 'disclosure' }, h('summary', null, 'HTTP request headers'), headersTable(shown, { noHighlight: true })));
              }
              row.appendChild(d);
            }
          };
          row.addEventListener('click', (ev) => { if (!ev.target.closest('.log-detail')) toggle(); });
          row.addEventListener('keydown', (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); toggle(); } });
          logEl.appendChild(row);
        }
        renderExport();
      });
    }

    function renderExport() {
      const parts = authParts();
      const ex = exportSession(st.log, { url: clientUrl(parts.query), transport: transportOf(st.version), authHeaders: parts.headers, name: 'rustybin' });
      const sel = select([['claude', 'claude mcp add'], ['hurl', 'Hurl'], ['curl', 'curl'], ['python', 'Python MCP SDK']], settings.recall('mcp.export', 'claude'), { 'aria-label': 'Export format', id: 'mcp-export-format' });
      const out = h('div');
      // Credentials are masked on screen; Copy gives the real command.
      const secrets = [...parts.headers.map(([, v]) => String(v).replace(/^[A-Za-z]+\s+/, '')), ...parts.query.map(([, v]) => String(v))].filter((v) => v.length > 3);
      const mask = (text) => secrets.reduce((t, v) => t.split(v).join(maskCredential(v)), text);
      const draw = () => { settings.remember('mcp.export', sel.value); const text = ex[sel.value] || ''; replace(out, text ? h('div', { class: 'stack-sm' }, h('div', { class: 'row' }, secrets.length ? h('span', { class: 'tiny muted' }, 'Credentials masked here; Copy includes them.') : null, h('span', { class: 'grow' }), copyButton(() => text, { label: 'Copy' })), codeBlock(mask(text), null, 'short')) : notice('', 'Send something first.')); };
      sel.addEventListener('change', draw);
      replace(exportEl, h('div', { class: 'stack-sm' }, sel, out));
      draw();
    }

    // ── Server requests (elicitation, sampling) ──
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
          const reply = textarea({ rows: 3, 'aria-label': 'Sampling reply', id: 'mcp-sampling-reply' });
          reply.value = '';
          const msgs = (p.messages || []).map((x) => `${x.role}: ${x.content && x.content.text ? x.content.text : JSON.stringify(x.content)}`).join('\n');
          const aiBtn = assist.enabled()
            ? h('button', { class: 'btn', type: 'button', id: 'mcp-sampling-ai', onclick: async () => {
              aiBtn.disabled = true;
              const r = await assist.run('sampling', { messages: (p.messages || []).map((x) => ({ role: x.role, content: x.content })), system_prompt: p.systemPrompt || null }, {});
              aiBtn.disabled = false;
              if (r.error) toast('The assistant failed: ' + r.error, 'err'); else reply.value = (r.text || '').trim();
            } }, icon('ai'), 'Answer with AI')
            : null;
          const why = assist.enabled() ? null : h('p', { class: 'small muted' }, 'No AI provider is set: type the reply, or reject. ', h('a', { href: '#/manage/ai' }, 'Add a key under Management, AI'), ' to answer with your model.');
          replace(pendingEl, card('The server asks the client to sample an LLM', { class: 'dialog-like' }, h('div', { class: 'stack-sm' },
            codeBlock(msgs || JSON.stringify(p, null, 2), null, 'short'), field('Reply', reply), why,
            h('div', { class: 'row' }, aiBtn,
              h('button', { class: 'btn primary', type: 'button', onclick: () => { clear(pendingEl); resolve({ result: { role: 'assistant', content: { type: 'text', text: reply.value || '(empty reply)' }, model: assist.enabled() ? ((assist.getStatus() || {}).model || 'assist') : 'rustybin-console', stopReason: 'endTurn' } }); } }, 'Send reply'),
              h('button', { class: 'btn', type: 'button', onclick: () => { clear(pendingEl); resolve({ error: { code: -1, message: 'User rejected sampling request' } }); } }, 'Reject')))));
        } else {
          resolve({ error: { code: -32601, message: 'Method not supported by the console: ' + method } });
        }
      });
    }

    // ── Connect ──
    async function connect() {
      if (client) await disconnect();
      const target = targetPath();
      if (!target) { replace(statusEl, notice('warn', 'Pick a server or type its URL.')); return; }
      connectBtn.disabled = true;
      replace(statusEl, spinner('connecting'));
      replace(mainEl, card(null, null, spinner('Connecting')));
      const parts = authParts();
      client = new McpClient({ url: clientUrl(parts.query), mode: st.mode, version: st.version, authHeaders: parts.headers, connection: st.source === 'saved' ? st.saved : null, log, onServerRequest: askUser });
      const res = await client.connect();
      connectBtn.disabled = false;
      if (!alive) return;
      if (res.error || !res.result) {
        const status = res.status;
        const www = res.headers && (res.headers.find(([k]) => String(k).toLowerCase() === 'www-authenticate') || [])[1];
        const networkError = !status && st.mode === 'browser';
        const relayRefused = res.error && res.error.reason === 'egress_refused';
        replace(statusEl, statusBadge(status || 0, 'failed'));
        replace(mainEl, card('Connection failed', null, h('div', { class: 'stack-sm' },
          notice('err', `${res.error.message || 'error'}${res.error.code ? ' (code ' + res.error.code + ')' : ''}`),
          status === 401 ? notice('warn', 'The server wants credentials. ', www ? h('code', { class: 'break' }, 'WWW-Authenticate: ' + www) : null, ' Pick OAuth 2.1 and Get a token for the discovery walk, or another auth mode.') : null,
          networkError ? notice('warn', 'The browser could not reach the server (CORS or the network). ', h('button', { class: 'btn sm primary', type: 'button', id: 'mcp-use-relay', onclick: () => { st.mode = 'relay'; modeSeg.set('relay'); connect(); } }, 'Connect through the relay')) : null,
          relayRefused ? notice('warn', 'The instance may not connect to this host. ', h('a', { href: '#/manage/network' }, 'Add it to the outbound allowlist under Management, Network'), ' (RUSTYBIN_EGRESS_ALLOW; loopback needs its own switch).') : null,
          res.error.data ? jsonView(res.error.data, 'short') : null)));
        client = null;
        return;
      }
      server = res.result;
      disconnectBtn.disabled = false;
      renderStatus();
      renderMain();
    }

    function renderStatus() {
      if (!client || !server) { replace(statusEl, badge('not connected')); return; }
      const info = (server._meta && server._meta['io.modelcontextprotocol/serverInfo']) || server.serverInfo || {};
      const stats = client.stats();
      replace(statusEl,
        h('div', { class: 'row' }, badge('connected', 'ok'), h('strong', { class: 'small' }, `${info.title || info.name || 'server'} ${info.version || ''}`)),
        kvgrid([
          ['Version', client.modern ? MODERN + ' (stateless)' : (server.protocolVersion || client.version)],
          ['Session', client.sessionId ? h('span', { class: 'mono small', title: 'Mcp-Session-Id' }, client.sessionId.slice(0, 16)) : client.relaySession ? h('span', { class: 'mono small', title: 'Relay session' }, 'relay ' + client.relaySession.slice(0, 10)) : 'none'],
          ['Mode', client.mode === 'relay' ? 'through the relay' : 'browser'],
          ['Auth', st.source === 'saved' && st.saved ? 'saved connection' : st.auth.kind],
          ['Latency', stats ? `p50 ${fmtMs(stats.p50)}, max ${fmtMs(stats.max)} (${stats.calls} calls)` : 'none yet'],
        ]));
    }

    async function disconnect() {
      if (callCtrl) callCtrl.abort();
      if (client) await client.close();
      client = null;
      server = null;
      disconnectBtn.disabled = true;
      replace(statusEl, badge('not connected'));
      replace(mainEl, notConnected());
    }

    function notConnected() {
      return card(null, null, h('div', { class: 'stack' }, empty('Not connected', 'Pick a server, a version and the auth mode, then Connect. Checks can run without connecting first.', 'mcp'),
        h('div', { class: 'row', style: { justifyContent: 'center' } }, h('button', { class: 'btn', type: 'button', onclick: () => { mainTab = 'checks'; renderMain(true); } }, icon('play'), 'Run checks instead'))));
    }

    // ── The protocol column ──
    let mainTab = settings.recall('mcp.tab', 'tools');
    function renderMain(checksOnly) {
      if (checksOnly && !server) {
        replace(mainEl, h('div', { class: 'card' }, h('div', { class: 'card-body' }, checksPane())));
        return;
      }
      const caps = server.capabilities || {};
      const t = tabs([
        { id: 'tools', label: 'Tools', render: toolsPane },
        { id: 'resources', label: 'Resources', render: resourcesPane },
        { id: 'prompts', label: 'Prompts', render: promptsPane },
        { id: 'server', label: 'Server', render: () => serverPane(caps) },
        { id: 'checks', label: 'Checks', render: () => h('div', { class: 'card-body' }, checksPane()) },
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
        renderStatus();
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
        replace(host, h('div', { class: 'stack' },
          h('div', { class: 'row' }, h('span', { class: 'small muted grow' }, `${tools.length} tools`),
            assist.assistButton('Write a test for this server', 'free_answer', () => ({ question: 'Write a Hurl test for this MCP server: discover or initialize, list the tools, call two of them with valid arguments and assert the results.', view: 'mcp', context: { server: targetPath(), version: st.version, tools: tools.slice(0, 30).map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })) } }))),
          list, detail));
        const first = tools.find((t) => t.name === 'slow_task') || tools[0];
        if (first) pick(first);
      });
    }

    function renderTool(host, t, initial) {
      const form = schemaForm(t.inputSchema, initial || (t.name === 'slow_task' ? { duration_ms: 3000, steps: 5 } : t.name === 'get_weather' ? { city: 'Paris' } : null));
      const result = h('div');
      const callBtn = h('button', { class: 'btn primary', type: 'button' }, icon('play'), 'Call tool');
      const cancelBtn = h('button', { class: 'btn', type: 'button', hidden: true }, icon('stop'), 'Cancel');
      callBtn.addEventListener('click', () => callTool(t, form, result, callBtn, cancelBtn));
      cancelBtn.addEventListener('click', () => { if (callCtrl) callCtrl.abort(); });
      const genBtn = h('button', { class: 'btn sm' + (assist.enabled() ? '' : ' assist-off'), type: 'button', id: 'mcp-generate-args', title: assist.enabled() ? 'Arguments from your AI provider, valid against the schema' : 'Add an AI key in Management, AI' }, icon('ai'), 'Generate arguments');
      genBtn.addEventListener('click', async () => {
        if (!assist.enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; }
        genBtn.disabled = true;
        const r = await assist.run('tool_arguments', { tool: { name: t.name, description: t.description, inputSchema: t.inputSchema || { type: 'object' } } }, {});
        genBtn.disabled = false;
        if (r.error || !r.json) { toast('No arguments came back: ' + (r.error || 'empty answer'), 'err'); return; }
        renderTool(host, t, r.json.arguments || {});
        toast('Arguments generated (validated against the schema)');
      });
      const explainBtn = assist.assistButton('Explain this tool', 'explain', () => ({ kind: 'tool', subject: { name: t.name, title: t.title, description: t.description, annotations: t.annotations, inputSchema: t.inputSchema, outputSchema: t.outputSchema } }));
      const ann = t.annotations || {};
      replace(host, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('h3', { class: 'mono' }, t.name), ann.destructiveHint ? badge('destructive', 'err') : null, ann.readOnlyHint ? badge('read only', 'ok') : null, t.title ? h('span', { class: 'muted small' }, t.title) : null,
          h('span', { class: 'grow' }), genBtn, explainBtn),
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
      // A cancelled call in a session: tell the server.
      if (callCtrl && callCtrl.signal.aborted && client && client.transport === 'session') client.notify('notifications/cancelled', { requestId: client.seq, reason: 'cancelled in the console' });
      callBtn.disabled = false;
      cancelBtn.hidden = true;
      callCtrl = null;
      if (!alive) return;
      renderStatus();
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
        const templatesEl = h('div');
        const subscribe = server.capabilities && server.capabilities.resources && server.capabilities.resources.subscribe;
        replace(host, h('div', { class: 'stack' },
          resources.length ? h('div', { class: 'list', style: { border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)' } }, ...resources.map((r) =>
            h('button', { class: 'list-item', type: 'button', style: { gridTemplateColumns: 'minmax(0,1fr) auto' }, onclick: () => read(r, detail, subscribe) },
              h('span', null, h('div', { class: 'mono small' }, r.uri), h('div', { class: 'tiny muted' }, r.description || r.title || r.name)), badge(r.mimeType || '', 'outline')))) : empty('No resources'),
          templatesEl, detail));
        (async () => {
          const res = await client.request('resources/templates/list', {});
          if (!alive || !client) return;
          const list = (res.result && res.result.resourceTemplates) || [];
          if (list.length) replace(templatesEl, h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Templates'), ...list.map((tpl) => h('div', { class: 'small' }, h('code', null, tpl.uriTemplate), tpl.description ? h('span', { class: 'muted' }, ' ' + tpl.description) : null))));
        })();
      });
    }

    async function read(r, detail, subscribe) {
      replace(detail, spinner('resources/read'));
      const res = await client.request('resources/read', { uri: r.uri });
      if (!alive) return;
      if (res.error) { replace(detail, notice('err', res.error.message)); return; }
      const subBtn = subscribe && client.transport === 'session'
        ? h('button', { class: 'btn sm', type: 'button', onclick: async () => { const s = await client.request('resources/subscribe', { uri: r.uri }); toast(s.error ? 'Subscribe failed: ' + s.error.message : 'Subscribed: updates appear in the timeline'); } }, 'Subscribe')
        : null;
      replace(detail, h('div', { class: 'stack-sm' }, subBtn ? h('div', { class: 'row' }, subBtn) : null, ...(res.result.contents || []).map((c) =>
        c.text !== undefined ? h('div', null, badge(c.mimeType || 'text', 'outline'), codeBlock(c.text, (c.mimeType || '').includes('json') ? 'json' : null, 'tall'))
          : c.blob && (c.mimeType || '').startsWith('image/') ? h('div', { class: 'artifact' }, badge(c.mimeType), h('img', { src: `data:${c.mimeType};base64,${c.blob}`, alt: c.uri }))
            : jsonView(c, 'short'))));
    }

    function promptsPane() {
      return listPane('prompts/list', 'prompts', (host, prompts) => {
        const detail = h('div');
        const list = h('div', { class: 'pill-list' }, ...prompts.map((p) => h('button', { class: 'btn sm', type: 'button', onclick: () => renderPrompt(p, detail) }, p.name)));
        replace(host, prompts.length ? h('div', { class: 'stack' }, list, detail) : empty('No prompts'));
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
      // Completion of the first argument, when the server offers it.
      const caps = server.capabilities || {};
      const firstArg = (p.arguments || [])[0];
      const complete = caps.completions && firstArg ? h('button', { class: 'btn sm', type: 'button', onclick: async () => {
        const res = await client.request('completion/complete', { ref: { type: 'ref/prompt', name: p.name }, argument: { name: firstArg.name, value: '' } });
        if (res.error) { toast('completion/complete: ' + res.error.message, 'err'); return; }
        const values = (res.result.completion && res.result.completion.values) || [];
        replace(out, notice('', h('strong', null, firstArg.name + ': '), values.length ? values.join(', ') : 'no suggestions'));
      } }, 'Suggest values for ' + firstArg.name) : null;
      replace(detail, h('div', { class: 'stack-sm' }, h('h3', { class: 'mono' }, p.name), p.description ? h('p', { class: 'small muted' }, p.description) : null, form.el, h('div', { class: 'row' }, btn, complete), out));
    }

    function serverPane(caps) {
      const out = h('div');
      const levelSel = select(['debug', 'info', 'notice', 'warning', 'error'].map((l) => [l, l]), 'info', { 'aria-label': 'Log level' });
      const logging = caps.logging ? h('div', { class: 'row' }, field('Server log level', levelSel), h('button', { class: 'btn sm', type: 'button', onclick: async () => { const r = await client.request('logging/setLevel', { level: levelSel.value }); replace(out, r.error ? notice('err', r.error.message) : notice('ok', 'Log level set to ' + levelSel.value + ': notifications/message appear in the timeline.')); } }, 'Set')) : null;
      const ping = h('button', { class: 'btn sm', type: 'button', id: 'mcp-ping', onclick: async () => { const r = await client.request('ping', {}); replace(out, r.error ? notice('err', r.error.message) : notice('ok', 'pong in ' + fmtMs(r.ms || 0))); renderStatus(); } }, 'Ping');
      const terminate = client.transport === 'session' && client.sessionId ? h('button', { class: 'btn sm danger', type: 'button', onclick: async () => { await disconnect(); toast('Session ended (DELETE)'); } }, 'End the session') : null;
      return h('div', { class: 'card-body stack-sm' },
        server.instructions ? h('p', { class: 'small' }, server.instructions) : null,
        kvgrid([['Capabilities', Object.keys(caps).join(', ') || 'none'], ['Versions', (server.supportedVersions || [server.protocolVersion]).filter(Boolean).join(', ')]]),
        h('div', { class: 'row' }, ping, terminate), logging, out,
        jsonView(server, 'tall'));
    }

    // ── Checks ──
    function checksPane() {
      const el = h('div', { class: 'stack', id: 'mcp-checks' });
      const chosen = new Set(settings.recall('mcp.checks', checksList.map((c) => c.id)));
      const boxes = h('div', { class: 'stack-sm' }, ...checksList.map((c) => {
        const cb = checkbox(h('span', null, h('code', null, c.id), ' ', h('span', { class: 'muted small' }, c.title)), chosen.has(c.id), (v) => { if (v) chosen.add(c.id); else chosen.delete(c.id); settings.remember('mcp.checks', [...chosen]); });
        return cb.el;
      }));
      const destructive = checkbox('Call destructive tools too', false);
      const ai = checkbox('Tool arguments from the AI provider', false);
      if (!assist.enabled()) { ai.input.disabled = true; ai.el.title = 'Add an AI key in Management, AI'; }
      const maxTools = input({ type: 'number', min: '1', max: '50', value: '25', 'aria-label': 'Most tools called', style: { maxWidth: '7em' } });
      const out = h('div', { id: 'mcp-checks-report' });
      const runBtn = h('button', { class: 'btn primary', type: 'button', id: 'mcp-run-checks' }, icon('play'), 'Run checks');
      const runsEl = h('div');
      runBtn.addEventListener('click', async () => {
        const target = targetPath();
        if (!target) { replace(out, notice('warn', 'Pick a server first.')); return; }
        runBtn.disabled = true;
        replace(out, spinner('Running the checks (each tool is called once)'));
        const parts = authParts();
        const hmap = {};
        for (const [k, v] of parts.headers) hmap[k] = v;
        const body = { target: withQuery(target, parts.query), transport: transportOf(st.version), version: transportOf(st.version) === 'session' ? st.version : undefined, headers: hmap, checks: [...chosen], include_destructive: destructive.input.checked, max_tools: Number(maxTools.value) || 25, ai_arguments: ai.input.checked };
        if (st.source === 'saved' && st.saved) body.connection = st.saved;
        const r = await send({ method: 'POST', url: SERVER + '/_rustybin/mcp-checks', headers: [['Content-Type', 'application/json']], body: JSON.stringify(body), timeout: 600000, session: false });
        runBtn.disabled = false;
        if (!alive) return;
        if (!r.ok || !r.json) { replace(out, notice('err', (r.json && r.json.error) || r.error || ('HTTP ' + r.status))); return; }
        replace(out, reportView(r.json));
        loadRuns();
      });
      async function loadRuns() {
        try {
          const d = await getJson('/_rustybin/mcp-checks', { session: false });
          const runs = d.runs || [];
          replace(runsEl, runs.length ? h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Saved runs'), ...runs.slice(0, 10).map((run) => h('button', { class: 'list-item', type: 'button', style: { gridTemplateColumns: 'auto minmax(0,1fr) auto' }, onclick: async () => { const rr = await getJson('/_rustybin/mcp-checks/' + encodeURIComponent(run.id), { session: false }); replace(out, reportView(rr)); } },
            badge(run.summary && run.summary.failed ? 'fail' : 'pass', run.summary && run.summary.failed ? 'err' : 'ok'), h('span', { class: 'mono small ellipsis' }, run.target), h('span', { class: 'tiny muted' }, fmtTime(run.started_at))))) : null);
        } catch { replace(runsEl); }
      }
      loadRuns();
      el.append(
        h('p', { class: 'small muted', style: { margin: 0 } }, 'A repeatable suite run by the instance against ', h('code', null, targetPath() || 'the server'), ' (in process for this instance\'s servers, through the outbound policy for others), with the version and credentials above.'),
        boxes,
        h('div', { class: 'row' }, destructive.el, ai.el, h('label', { class: 'row nowrap small' }, 'Most tools', maxTools)),
        h('div', { class: 'row' }, runBtn), out, runsEl);
      return el;
    }

    function reportView(r) {
      const s = r.summary || {};
      const download = (name, text, type) => { const a = h('a', { href: URL.createObjectURL(new Blob([text], { type })), download: name }); document.body.appendChild(a); a.click(); a.remove(); };
      const junitBtn = h('button', { class: 'btn sm', type: 'button', id: 'mcp-checks-junit', onclick: async () => {
        const rr = await send({ url: SERVER + '/_rustybin/mcp-checks/' + encodeURIComponent(r.id) + '?format=junit', session: false });
        if (rr.ok) download('mcp-checks.xml', rr.text, 'application/xml'); else toast('JUnit export failed', 'err');
      } }, icon('download'), 'JUnit');
      const jsonBtn = h('button', { class: 'btn sm', type: 'button', onclick: () => download('mcp-checks.json', JSON.stringify(r, null, 2), 'application/json') }, icon('download'), 'JSON');
      const rows = (r.checks || []).map((c) => h('tr', null,
        h('td', null, badge(c.status, c.status === 'pass' ? 'ok' : c.status === 'fail' ? 'err' : c.status === 'warn' ? 'warn' : 'outline')),
        h('td', null, h('div', { class: 'mono small' }, c.id), h('div', { class: 'tiny muted' }, c.title)),
        h('td', { class: 'small' }, c.message, c.details && c.details.calls ? h('details', { class: 'disclosure' }, h('summary', null, c.details.calls.length + ' calls'), jsonView(c.details.calls, 'short')) : null),
        h('td', { class: 'small mono' }, c.duration_ms ? c.duration_ms + ' ms' : '')));
      return h('div', { class: 'stack-sm' },
        h('div', { class: 'row' },
          badge(r.ok ? 'passed' : 'failed', r.ok ? 'ok' : 'err'),
          h('span', { class: 'small' }, `${s.passed || 0} passed, ${s.warnings || 0} warnings, ${s.failed || 0} failed, ${s.skipped || 0} skipped in ${fmtMs(r.duration_ms || 0)}`),
          r.relayed ? badge('through the outbound policy', 'outline') : badge('in process', 'outline'),
          h('span', { class: 'grow' }), jsonBtn, junitBtn),
        r.latency ? h('div', { class: 'small muted' }, `Latency: p50 ${r.latency.p50_ms} ms, p95 ${r.latency.p95_ms} ms, max ${r.latency.max_ms} ms over ${r.latency.calls} calls`) : null,
        h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'mcp-checks-table' },
          h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Check'), h('th', null, 'Result'), h('th', null, ''))),
          h('tbody', null, rows))));
    }

    // ── Data ──
    async function loadServers() {
      try {
        const d = await getJson('/_rustybin/mcp-servers', { session: false });
        servers = d.servers || [];
        checksList = d.checks || [];
        relayInfo = d.relay || null;
      } catch (e) {
        servers = [{ path: '/mcp', title: 'Mock MCP server (open)', transports: ['modern', 'session'], auth: { kind: 'none' }, state: 'serving' }];
        console.warn('mcp servers unavailable', e);
      }
    }

    async function loadSaved() {
      try { saved = (await getJson('/_rustybin/mcp-connections', { session: false })).connections || []; } catch { saved = []; }
    }

    function route(r) {
      const m = /^server\/(.+)$/.exec(r || '');
      if (m) {
        const path = decodeURIComponent(m[1]);
        st.source = 'instance';
        st.server = path;
        sourceSeg.set('instance');
        applyServerDefaults(serverInfo());
        renderLeft();
      }
    }

    replace(mainEl, notConnected());
    renderLog();
    (async () => {
      await Promise.all([loadServers(), loadSaved()]);
      if (!alive) return;
      if (!servers.some((s) => s.path === st.server)) st.server = '/mcp';
      renderLeft();
      route(rest);
    })();

    const cleanup = () => { alive = false; if (callCtrl) callCtrl.abort(); if (client) client.close(); };
    cleanup.onRoute = route;
    cleanup.destroy = cleanup;
    cleanup.assistContext = () => ({ server: targetPath(), version: st.version, mode: st.mode, connected: !!server, capabilities: server && server.capabilities });
    return cleanup;
  },
};
