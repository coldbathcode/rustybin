// MCP registry: the mock registry this instance serves at /mcp-registry
// (its entries with their source, the scenario that breaks a gateway's
// registry sync on purpose, a publish form that validates as you type, the
// base URL and a sync snippet to point a gateway at) and a browser for real
// registries (search, pages, remotes and packages, Open in the inspector,
// Validate). AI: "Explain this server entry" and "Write a server.json for my
// mock". Everything a registry answers is shown as text.
import { h, replace, clear, card, field, input, select, notice, empty, icon, badge, toast, kvgrid, textarea, copyButton, debounce, spinner, fmtTime } from '../lib/dom.js';
import { copyableCode } from '../lib/format.js';
import { SERVER, send, getJson } from '../lib/http.js';
import * as settings from '../lib/settings.js';
import * as assist from '../lib/assist.js';

const DEFAULT_REGISTRY = 'https://registry.modelcontextprotocol.io';
const LOCAL = '/mcp-registry';
const AUTH_STUBS = [
  ['none', 'none (anonymous)'],
  ['github-at', 'github-at (a GitHub token)'],
  ['github-oidc', 'github-oidc (an Actions OIDC token)'],
  ['oidc', 'oidc (an OIDC ID token)'],
  ['dns', 'dns (a signed DNS challenge)'],
  ['http', 'http (a signed HTTP challenge)'],
];
const STUB_BODIES = {
  none: {},
  'github-at': { github_token: 'gho_console' },
  'github-oidc': { oidc_token: 'console' },
  oidc: { oidc_token: 'console' },
  dns: { domain: 'example.com', timestamp: '', signed_timestamp: 'console' },
  http: { domain: 'example.com', timestamp: '', signed_timestamp: 'console' },
};

const TEMPLATE = {
  $schema: 'https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json',
  name: 'io.example/my-server',
  title: 'My server',
  description: 'What the tools of this server do, in at most 100 characters',
  version: '1.0.0',
  remotes: [{ type: 'streamable-http', url: 'https://mcp.example.com/mcp' }],
};

function pretty(v) { return JSON.stringify(v, null, 2); }

function sourceBadge(s) {
  return badge(s, s === 'published' ? 'info' : s === 'file' ? 'ok' : s === 'scenario' ? 'warn' : 'outline');
}

function statusBadgeOf(s) {
  return badge(s || 'active', s === 'deleted' ? 'err' : s === 'deprecated' ? 'warn' : 'ok');
}

function issuesList(issues) {
  const list = issues || [];
  const valid = !list.some((i) => i.severity === 'error');
  return h('div', { class: 'stack-sm' },
    valid ? h('div', { class: 'small muted inline-icon' }, icon('check'), ' Valid against the server.json schema' + (list.length ? ', with warnings:' : '.')) : null,
    list.length ? h('ul', { class: 'small stack-sm', style: { margin: 0, paddingLeft: '18px' } },
      list.map((i) => h('li', null, badge(i.severity, i.severity === 'error' ? 'err' : 'warn'), ' ', h('span', { class: 'mono' }, i.path || '(document)'), ': ', i.message))) : null);
}

/** The inspector's path for a remote on this instance, else null. */
function localPath(url, hint) {
  if (hint && typeof hint.path === 'string' && hint.path.startsWith('/')) return hint.path;
  try {
    const u = new URL(url, location.href);
    const here = new URL(SERVER || location.origin, location.href);
    if (u.origin === here.origin && /^\/(mcp|mock\/|_rustybin\/mcp)/.test(u.pathname)) return u.pathname;
  } catch { /* not a URL */ }
  return null;
}

/** Open a remote in the MCP inspector, with the auth it declares. */
function openInInspector(remote, hint) {
  const path = localPath(remote.url, hint);
  if (path) { location.hash = '#/mcp/server/' + encodeURIComponent(path); return; }
  const a = remote.auth || { kind: 'none' };
  const kind = ['none', 'bearer', 'api_key', 'basic'].includes(a.kind) ? a.kind : 'none';
  location.hash = '#/mcp/url/' + [kind, encodeURIComponent(a.name || '-'), remote.type === 'sse' ? 'legacy' : 'modern', encodeURIComponent(remote.url)].join('/');
}

/** Remotes of a server.json as the browse route summarises them. */
function remotesOf(server) {
  return (Array.isArray(server && server.remotes) ? server.remotes : []).map((r) => {
    const headers = Array.isArray(r.headers) ? r.headers : [];
    let auth = { kind: 'none' };
    for (const x of headers) {
      const n = String(x.name || '');
      if (n.toLowerCase() === 'authorization') { auth = { kind: /^\s*basic/i.test(String(x.value || '')) ? 'basic' : 'bearer' }; break; }
      if (n && x.isSecret) { auth = { kind: 'api_key', name: n }; break; }
    }
    return { type: r.type, url: r.url, headers: headers.map((x) => x.name).filter(Boolean), auth };
  });
}

function remoteRow(r, hint) {
  return h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '6px' } },
    badge(r.type || '?', 'outline'),
    h('span', { class: 'mono small ellipsis', style: { minWidth: 0, flex: '1 1 240px' } }, String(r.url || '')),
    r.auth && r.auth.kind !== 'none' ? badge('auth: ' + r.auth.kind + (r.auth.name ? ' ' + r.auth.name : ''), 'warn') : null,
    r.url ? h('button', { class: 'btn sm', type: 'button', 'data-action': 'inspect', onclick: () => openInInspector(r, hint) }, icon('mcp'), 'Open in inspector') : null);
}

async function validateDoc(doc) {
  const r = await send({ url: SERVER + '/_rustybin/registry/validate', method: 'POST', headers: { 'Content-Type': 'application/json' }, body: typeof doc === 'string' ? doc : JSON.stringify(doc), session: false });
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error((r.json && r.json.error) || r.status + ' ' + r.statusText);
  return r.json;
}

async function saveSetting(key, value) {
  const r = await send({ url: SERVER + '/_rustybin/settings', method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ values: { [key]: value } }), session: false });
  if (!r.ok) throw new Error((r.json && (r.json.error || (r.json.problems && r.json.problems.join('; ')))) || r.status + ' ' + r.statusText);
  return r.json;
}

export default {
  id: 'registry',
  mount(root, { rest }) {
    let alive = true;
    const st = settings.recall('registry.v1', null) || { tab: 'mock', registry: DEFAULT_REGISTRY, url: '', search: '', version: 'latest', doc: pretty(TEMPLATE), stub: 'none', mock: '' };
    settings.remember('registry.v1', st);
    let info = null;
    let infoError = null;

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'MCP registry'),
        h('p', null, 'A mock MCP registry for gateways and clients that discover servers through one: this instance\'s MCP servers, the entries of its config file and the ones you publish, in the registry API\'s shape, with scenarios that make it fail on purpose. And a browser for real registries: open any entry in the inspector, validate any server.json.'))));
    const tabsHost = h('div');
    root.appendChild(tabsHost);
    const mockEl = h('div', { class: 'stack' });
    const browseEl = h('div', { class: 'stack' });
    const t = tabsOf();
    tabsHost.appendChild(t.el);

    function tabsOf() {
      const bar = h('div', { class: 'tabs', role: 'tablist' });
      const body = h('div', { role: 'tabpanel' });
      const items = [['mock', 'Mock registry', mockEl], ['browse', 'Browse a registry', browseEl]];
      const buttons = items.map(([id, label]) => {
        const b = h('button', { type: 'button', role: 'tab', 'aria-selected': 'false', id: 'registry-tab-' + id }, label);
        b.addEventListener('click', () => select(id));
        bar.appendChild(b);
        return [id, b];
      });
      function select(id) {
        st.tab = id;
        for (const [bid, b] of buttons) { b.setAttribute('aria-selected', String(bid === id)); b.tabIndex = bid === id ? 0 : -1; }
        replace(body, items.find(([x]) => x === id)[2]);
        if (id === 'browse' && !browseEl.childElementCount) renderBrowse();
      }
      return { el: h('div', null, bar, body), select };
    }

    // ── Mock registry ──
    async function loadInfo() {
      try { info = await getJson('/_rustybin/registry', { session: false }); infoError = null; } catch (e) { infoError = e; }
    }

    async function reload() {
      await loadInfo();
      if (alive) renderMock();
    }

    function renderMock() {
      clear(mockEl);
      if (infoError) { mockEl.appendChild(notice('err', 'The mock registry could not be read: ' + infoError.message)); return; }
      if (!info) { mockEl.appendChild(spinner('Loading the mock registry')); return; }
      mockEl.appendChild(pointCard());
      mockEl.appendChild(entriesCard());
      mockEl.appendChild(publishCard());
    }

    function pointCard() {
      const base = info.base_url;
      const scenarioSel = select(info.scenarios.map((s) => [s.id, s.id + ': ' + s.title]), info.scenario, { id: 'registry-scenario', 'aria-label': 'Mock registry scenario' });
      scenarioSel.addEventListener('change', async () => {
        try { await saveSetting('registry.scenario', scenarioSel.value); toast('Scenario: ' + scenarioSel.value); reload(); }
        catch (e) { toast('The scenario was not changed: ' + e.message, 'err'); scenarioSel.value = info.scenario; }
      });
      const curl = `curl -s '${base}/v0.1/servers?version=latest&limit=30'`;
      return card('Point a gateway here', { hint: 'the registry API, /v0.1 and /v0',
        actions: copyButton(() => pretty(info.sync_config), { label: 'Copy as registry sync config', toast: 'Sync config copied' }) },
      h('div', { class: 'stack-sm' },
        kvgrid([
          ['Registry URL', h('span', { class: 'row nowrap' }, h('span', { class: 'mono', id: 'registry-base' }, base), copyButton(base, { icon: true, title: 'Copy the registry URL' }))],
          ['List servers', h('span', { class: 'mono small' }, base + '/v0.1/servers')],
          ['Schema', h('span', { class: 'mono small' }, info.schema)],
          ['Entries', `${info.counts.generated} generated, ${info.counts.file} from the config file, ${info.counts.published} published (at most ${info.capacity} file and published)` + (info.counts.scenario ? `, ${info.counts.scenario} from the scenario` : '')],
        ]),
        field('Scenario', scenarioSel, 'What the registry does to a client. Saved as the registry.scenario setting (admin)' + (info.scenario_source === 'env' ? '; RUSTYBIN_REGISTRY_SCENARIO sets it now.' : '.')),
        copyableCode(curl, null, 'small')));
    }

    function entriesCard() {
      const rows = info.entries.map((e) => entryRow(e));
      const table = h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'registry-entries' },
        h('thead', null, h('tr', null, h('th', null, 'Server'), h('th', null, 'Version'), h('th', null, 'Source'), h('th', null, 'Status'), h('th', null, ''))),
        h('tbody', null, rows)));
      return card('Entries', { hint: 'what /v0.1/servers lists', actions: h('button', { class: 'btn sm', type: 'button', onclick: reload }, icon('refresh'), 'Refresh') },
        info.entries.length ? table : empty('No entries', info.scenario === 'empty' ? 'The empty scenario hides every entry.' : 'Nothing is listed.', 'data'),
        info.counts.scenario && info.scenario === 'large' ? h('p', { class: 'small muted' }, `Plus ${info.counts.scenario} synthetic entries of the large scenario (listed by the API, not here).`) : null);
    }

    function entryRow(e) {
      const detail = h('tr', { hidden: true }, h('td', { colspan: '5' }));
      const hint = e.server && e.server._meta && e.server._meta['io.modelcontextprotocol.registry/publisher-provided'] && e.server._meta['io.modelcontextprotocol.registry/publisher-provided'].rustybin;
      const showJson = () => {
        detail.hidden = !detail.hidden;
        replace(detail.firstChild, h('div', { class: 'stack-sm' },
          remotesOf(e.server).map((r) => remoteRow(r, hint)),
          e.issues && e.issues.length ? issuesList(e.issues) : null,
          copyableCode(pretty(e.server), 'json')));
      };
      const del = e.source === 'published'
        ? h('button', { class: 'btn sm', type: 'button', title: 'Remove this version', 'aria-label': 'Remove ' + e.name + ' ' + e.version, onclick: async () => {
          const r = await send({ url: SERVER + '/_rustybin/registry/servers/' + encodeURIComponent(e.name) + '?version=' + encodeURIComponent(e.version), method: 'DELETE', session: false });
          if (r.ok) { toast('Removed ' + e.name + ' ' + e.version); reload(); } else toast('Not removed: ' + ((r.json && r.json.error) || r.status), 'err');
        } }, icon('trash'))
        : null;
      const row = h('tr', { 'data-name': e.name },
        h('td', null, h('div', { class: 'mono small' }, e.name), h('div', { class: 'tiny muted ellipsis' }, e.description || ''),
          e.valid === false ? badge('invalid: ' + (e.issues || []).filter((i) => i.severity === 'error').length + ' errors', 'err') : null),
        h('td', { class: 'mono small' }, e.version, e.isLatest ? h('span', { class: 'tiny muted' }, ' latest') : null),
        h('td', null, sourceBadge(e.source)),
        h('td', null, statusBadgeOf(e.status)),
        h('td', null, h('div', { class: 'row nowrap', style: { justifyContent: 'flex-end' } },
          h('button', { class: 'btn sm', type: 'button', onclick: showJson }, 'server.json'),
          remotesOf(e.server)[0] ? h('button', { class: 'btn sm', type: 'button', 'data-action': 'inspect', onclick: () => openInInspector(remotesOf(e.server)[0], hint) }, icon('mcp'), 'Inspect') : null,
          assist.assistButton('Explain', 'explain', () => ({ kind: 'server_entry', subject: { name: e.name, server: e.server, source: e.source, status: e.status } })),
          del)));
      return [row, detail];
    }

    function publishCard() {
      const doc = textarea({ rows: 14, class: 'textarea mono', id: 'registry-publish-doc', 'aria-label': 'server.json to publish', spellcheck: 'false' });
      doc.value = st.doc;
      const issuesEl = h('div', { id: 'registry-publish-issues' });
      const stubSel = select(AUTH_STUBS, st.stub, { id: 'registry-publish-auth', 'aria-label': 'Registry auth method' });
      stubSel.addEventListener('change', () => { st.stub = stubSel.value; });
      const resultEl = h('div');
      const publishBtn = h('button', { class: 'btn primary', type: 'button', id: 'registry-publish' }, icon('upload'), 'Publish');
      const check = debounce(async () => {
        st.doc = doc.value;
        let parsed;
        try { parsed = JSON.parse(doc.value); } catch (e) { replace(issuesEl, notice('err', 'Not JSON yet: ' + e.message)); return; }
        try { const v = await validateDoc(parsed); if (alive) replace(issuesEl, issuesList(v.issues)); } catch (e) { replace(issuesEl, notice('err', 'Validation failed: ' + e.message)); }
      }, 300);
      doc.addEventListener('input', check);
      check();
      publishBtn.addEventListener('click', async () => {
        let body;
        try { body = JSON.parse(doc.value); } catch (e) { toast('Not JSON: ' + e.message, 'err'); return; }
        publishBtn.disabled = true;
        try {
          const stub = Object.assign({}, STUB_BODIES[st.stub] || {});
          if ('timestamp' in stub) stub.timestamp = new Date().toISOString();
          const tok = await send({ url: SERVER + '/mcp-registry/v0.1/auth/' + st.stub, method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(stub), session: false });
          if (!tok.ok) { replace(resultEl, notice('err', 'Token exchange (' + st.stub + ') answered ' + tok.status + ': ' + ((tok.json && tok.json.detail) || tok.text.slice(0, 200)))); return; }
          const r = await send({ url: SERVER + '/mcp-registry/v0.1/publish', method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok.json.registry_token }, body: JSON.stringify(body), session: false });
          if (r.ok) { replace(resultEl, notice('ok', 'Published ' + r.json.server.name + ' ' + r.json.server.version + '.')); reload(); }
          else replace(resultEl, notice('err', 'Publish answered ' + r.status + ': ' + ((r.json && r.json.detail) || r.text.slice(0, 300))));
        } finally { publishBtn.disabled = false; }
      });
      // AI: a server.json for one of the instance's mocks, from the generated draft.
      const mocks = info.entries.filter((e) => e.source === 'generated' && /^io\.rustybin\/mock-/.test(e.name));
      const mockSel = select(mocks.length ? mocks.map((e) => [e.name, e.name.replace('io.rustybin/mock-', '')]) : [['', 'No deployed mock']], st.mock || (mocks[0] && mocks[0].name) || '', { id: 'registry-ai-mock', 'aria-label': 'Mock for the server.json' });
      mockSel.addEventListener('change', () => { st.mock = mockSel.value; });
      const writeBtn = h('button', { class: 'btn sm' + (assist.enabled() ? '' : ' assist-off'), type: 'button', id: 'registry-write-server-json', disabled: !mocks.length, title: assist.enabled() ? 'A server.json from your AI provider, validated against the schema' : 'Add an AI key in Management, AI' }, icon('ai'), 'Write a server.json for my mock');
      writeBtn.addEventListener('click', async () => {
        if (!assist.enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; }
        const entry = mocks.find((e) => e.name === mockSel.value);
        if (!entry) return;
        const name = entry.name.replace('io.rustybin/mock-', '');
        let tools = [];
        try { const d = await getJson('/_rustybin/mocks/' + encodeURIComponent(name), { session: false }); tools = ((d.mcp && d.mcp.tools) || []).slice(0, 40).map((x) => ({ name: x.name, description: x.description })); } catch { /* the draft alone */ }
        writeBtn.disabled = true;
        const r = await assist.run('server_json', { draft: entry.server, tools }, {});
        writeBtn.disabled = false;
        if (r.error || !r.json || !r.json.server) { toast('No server.json came back: ' + (r.error || 'empty answer'), 'err'); return; }
        doc.value = pretty(r.json.server);
        check();
        replace(resultEl, (r.json.notes || []).length ? notice('', h('div', null, 'Written for ' + name + ' (validated against the schema). ', (r.json.notes || []).join(' '))) : h('div'));
      });
      return card('Publish a server.json', { hint: 'validated as you type; a token from an auth stub first' },
        h('div', { class: 'stack-sm' },
          doc, issuesEl,
          h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } },
            field('Auth stub', stubSel, 'Every stub succeeds unless the scenario is auth_fail.'), h('span', { class: 'grow' }), publishBtn),
          h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } }, mockSel, writeBtn,
            h('button', { class: 'btn sm', type: 'button', onclick: () => { doc.value = pretty(TEMPLATE); check(); } }, 'Reset to the template')),
          resultEl));
    }

    // ── Browse a registry ──
    let cursors = [];
    let page = null;

    function registryOptions() {
      const saved = (info && info.saved) || [];
      const opts = [[DEFAULT_REGISTRY, DEFAULT_REGISTRY], [LOCAL, 'This instance\'s mock registry (' + LOCAL + ')']];
      for (const s of saved) if (s !== DEFAULT_REGISTRY && s !== LOCAL) opts.push([s, s]);
      opts.push(['other', 'Another registry URL']);
      return opts;
    }

    function renderBrowse() {
      clear(browseEl);
      const regSel = select(registryOptions(), registryOptions().some(([v]) => v === st.registry) ? st.registry : DEFAULT_REGISTRY, { id: 'registry-browse-registry', 'aria-label': 'Registry' });
      const urlIn = input({ value: st.url, placeholder: 'https://registry.example.com', id: 'registry-browse-url', class: 'input mono', 'aria-label': 'Registry URL' });
      const urlField = field('Registry URL', urlIn, 'Hosts other than registry.modelcontextprotocol.io and the saved registries need RUSTYBIN_EGRESS_ALLOW (Management, Network).');
      urlField.hidden = regSel.value !== 'other';
      regSel.addEventListener('change', () => { st.registry = regSel.value; urlField.hidden = regSel.value !== 'other'; });
      urlIn.addEventListener('input', () => { st.url = urlIn.value.trim(); });
      const searchIn = input({ value: st.search, placeholder: 'part of a server name (3 characters or more)', id: 'registry-browse-search', 'aria-label': 'Search' });
      searchIn.addEventListener('input', () => { st.search = searchIn.value; });
      searchIn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { cursors = []; go(null); } });
      const versionSel = select([['latest', 'Latest versions'], ['', 'Every version']], st.version, { 'aria-label': 'Versions', id: 'registry-browse-version' });
      versionSel.addEventListener('change', () => { st.version = versionSel.value; });
      const goBtn = h('button', { class: 'btn primary', type: 'button', id: 'registry-browse-go' }, icon('refresh'), 'Browse');
      goBtn.addEventListener('click', () => { cursors = []; go(null); });
      const saveBtn = h('button', { class: 'btn sm', type: 'button', title: 'Add this registry to registry.saved (admin)' }, icon('plus'), 'Save this registry');
      saveBtn.addEventListener('click', async () => {
        const url = target();
        if (!url || url === DEFAULT_REGISTRY || url === LOCAL) { toast('Pick another registry URL to save'); return; }
        const saved = Array.from(new Set(((info && info.saved) || []).concat([url])));
        try { await saveSetting('registry.saved', saved); toast('Saved ' + url); await loadInfo(); renderBrowse(); } catch (e) { toast('Not saved: ' + e.message, 'err'); }
      });
      const resultsEl = h('div', { class: 'stack', id: 'registry-browse-results' });
      browseEl.appendChild(card('Registry', { hint: 'fetched by this instance, cached for a minute', actions: saveBtn },
        h('div', { class: 'stack-sm' },
          h('div', { class: 'grid grid-2' }, field('Registry', regSel), field('Versions', versionSel)),
          urlField,
          h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, searchIn), goBtn))));
      browseEl.appendChild(resultsEl);
      if (page) renderPage(resultsEl);
      else resultsEl.appendChild(empty('Pick a registry and browse', 'Entries come with their remotes and packages; open any remote in the inspector.', 'data'));

      function target() { return regSel.value === 'other' ? urlIn.value.trim() : regSel.value; }

      async function go(cursor) {
        const url = target();
        if (!url) { toast('Enter a registry URL'); return; }
        const q = new URLSearchParams({ url, limit: '30' });
        if (st.search.trim()) q.set('search', st.search.trim());
        if (st.version) q.set('version', st.version);
        if (cursor) q.set('cursor', cursor);
        replace(resultsEl, spinner('Fetching ' + url));
        const r = await send({ url: SERVER + '/_rustybin/registry/browse?' + q.toString(), session: false, timeout: 30000 });
        if (!alive) return;
        if (!r.ok) {
          const reason = r.json && r.json.reason;
          page = null;
          replace(resultsEl, notice('err', (r.json && r.json.error) || r.error || (r.status + ' ' + r.statusText),
            reason === 'egress_refused' ? h('div', { class: 'small' }, 'Allow the host under ', h('a', { href: '#/manage/network' }, 'Management, Network'), ' (or save the registry here).') : null));
          return;
        }
        page = { data: r.json, cursor };
        renderPage(resultsEl);
      }

      function renderPage(host) {
        clear(host);
        const d = page.data;
        const local = String(d.registry || '').startsWith('/');
        const nav = h('div', { class: 'row' },
          h('span', { class: 'small muted' }, `${d.count} ${d.count === 1 ? 'entry' : 'entries'} from ${d.registry}`, d.cached ? ' (cached)' : '', d.fetched_at ? ', fetched ' + fmtTime(d.fetched_at) : ''),
          h('span', { class: 'grow' }),
          h('button', { class: 'btn sm', type: 'button', disabled: !cursors.length, onclick: () => { const prev = cursors.pop(); go(prev === undefined ? null : prev); } }, 'Previous page'),
          h('button', { class: 'btn sm', type: 'button', id: 'registry-browse-next', disabled: !d.next_cursor, onclick: () => { cursors.push(page.cursor); go(d.next_cursor); } }, 'Next page'));
        host.appendChild(nav);
        if (!d.servers.length) { host.appendChild(empty('No entries', 'Nothing matches on this page.', 'data')); return; }
        for (const s of d.servers) host.appendChild(browseEntry(s, local));
      }
    }

    function browseEntry(s, local) {
      const issuesEl = h('div');
      const jsonEl = h('div', { hidden: true }, copyableCode(pretty(s.server), 'json'));
      const hint = local ? s.rustybin : null;
      return card(null, { class: 'registry-entry' },
        h('div', { class: 'stack-sm' },
          h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '6px' } },
            h('strong', { class: 'mono' }, String(s.name || '?')), badge(String(s.version || '?'), 'outline'),
            s.isLatest ? badge('latest', 'ok') : null, statusBadgeOf(s.status),
            s.valid === false ? badge('invalid', 'err') : null,
            h('span', { class: 'grow' }),
            h('button', { class: 'btn sm', type: 'button', 'data-action': 'validate', onclick: async () => {
              try { const v = await validateDoc(s.server); replace(issuesEl, issuesList(v.issues)); } catch (e) { replace(issuesEl, notice('err', e.message)); }
            } }, icon('check'), 'Validate'),
            h('button', { class: 'btn sm', type: 'button', onclick: () => { jsonEl.hidden = !jsonEl.hidden; } }, 'server.json'),
            assist.assistButton('Explain this server entry', 'explain', () => ({ kind: 'server_entry', subject: { name: s.name, server: s.server, status: s.status } }))),
          s.title || s.description ? h('div', { class: 'small' }, s.title ? h('strong', null, String(s.title) + ' ') : null, String(s.description || '')) : null,
          (s.remotes || []).map((r) => remoteRow(r, hint)),
          (s.packages || []).length ? h('div', { class: 'small' }, h('span', { class: 'muted' }, 'Packages: '),
            s.packages.map((p, i) => h('span', { class: 'mono' }, (i ? ', ' : '') + [p.registryType, p.identifier, p.version, p.transport].filter(Boolean).join(' ')))) : null,
          !(s.remotes || []).length && (s.packages || []).length ? h('div', { class: 'tiny muted' }, 'Packages run locally (stdio): the inspector connects to remotes only.') : null,
          issuesEl, jsonEl));
    }

    function route(r) {
      const id = r === 'browse' ? 'browse' : r === 'mock' ? 'mock' : null;
      if (id) t.select(id);
    }

    renderMock();
    t.select(rest === 'browse' ? 'browse' : rest === 'mock' ? 'mock' : st.tab || 'mock');
    (async () => {
      await loadInfo();
      if (!alive) return;
      renderMock();
      if (st.tab === 'browse') renderBrowse();
    })();

    const cleanup = () => { alive = false; };
    cleanup.onRoute = route;
    cleanup.destroy = cleanup;
    cleanup.assistContext = () => ({ tab: st.tab, scenario: info && info.scenario, entries: info && info.entries.map((e) => ({ name: e.name, version: e.version, source: e.source, status: e.status, valid: e.valid })), browsing: page && { registry: page.data.registry, count: page.data.count } });
    return cleanup;
  },
};
