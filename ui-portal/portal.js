// The dev portal: a read-only reader for what the operator published. It
// reads one endpoint, GET api/catalog (published items only), plus the
// public spec of each mock and api/items/<id>/collections/<format> for
// downloads; it reports views and Try it calls to api/events. It reuses the
// console's DOM helpers, formatters and stylesheet (../ui/) but none of its
// control-plane code: the only credential it holds is the one portal.auth
// asks for (the console sign-in in password mode, an API key in key mode),
// kept in this tab. Routes: #/ (home), #/k/<kind>, #/i/<item id>.
import { h, icon, replace, clear, notice, toast, badge, card, spinner, empty, copyButton, methodBadge, statusBadge, field, input, textarea, select, kvgrid, fmtMs } from '../ui/lib/dom.js';
import { codeBlock, copyableCode, bodyView, headersTable } from '../ui/lib/format.js';
import { renderMarkdown } from '../ui/lib/markdown.js';

const meta = (name) => {
  const el = document.querySelector('meta[name="rustybin-portal-' + name + '"]');
  return el ? el.getAttribute('content') || '' : '';
};
const AUTH = meta('auth') || 'open';
const SIGNIN = meta('signin') === 'true';
const CRED_KEY = 'rustybin-portal-credential';
const THEME_KEY = 'rustybin-portal-theme';
const KEY_HEADER = 'X-Rustybin-Key';
const KIND_ICONS = { api: 'mock', mcp: 'mcp', llm: 'ai', agent: 'a2a', tools: 'explorer', registry: 'data' };
const KIND_LABELS = { api: 'API', mcp: 'MCP server', llm: 'LLM', agent: 'Agent', tools: 'Tools', registry: 'Registry' };
const FORMAT_LABELS = { postman: 'Postman', hurl: 'Hurl', curl: 'curl', http: '.http', k6: 'k6', bruno: 'Bruno', insomnia: 'Insomnia' };

const viewEl = document.getElementById('view');
const searchEl = document.getElementById('search');
const footEl = document.getElementById('foot');
let catalog = null;
let etag = null;

// ── Storage (every access may throw in a locked-down browser) ──────

function store(kind) {
  try { return kind === 'local' ? window.localStorage : window.sessionStorage; } catch { return null; }
}
function load(kind, key) {
  try { const s = store(kind); return s ? s.getItem(key) : null; } catch { return null; }
}
function save(kind, key, value) {
  try {
    const s = store(kind);
    if (!s) return;
    if (value === null || value === undefined) s.removeItem(key); else s.setItem(key, value);
  } catch { /* storage unavailable */ }
}

// ── Theme and branding ─────────────────────────────────────────────

function applyTheme() {
  const t = load('local', THEME_KEY);
  if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
  else document.documentElement.removeAttribute('data-theme');
}

function effectiveTheme() {
  const t = load('local', THEME_KEY);
  if (t === 'light' || t === 'dark') return t;
  return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function topActions() {
  const actions = document.getElementById('top-actions');
  const theme = h('button', { class: 'btn ghost icon', type: 'button', id: 'theme-btn' });
  const paint = () => {
    const dark = effectiveTheme() === 'dark';
    replace(theme, icon(dark ? 'sun' : 'moon'));
    theme.setAttribute('aria-label', dark ? 'Switch to light theme' : 'Switch to dark theme');
    theme.title = theme.getAttribute('aria-label');
  };
  theme.addEventListener('click', () => {
    save('local', THEME_KEY, effectiveTheme() === 'dark' ? 'light' : 'dark');
    applyTheme();
    paint();
  });
  paint();
  const signOut = credential() ? h('button', { class: 'btn sm ghost', type: 'button', id: 'portal-sign-out', onclick: () => { save('session', CRED_KEY, null); catalog = null; route(); topActions(); } }, 'Sign out') : null;
  replace(actions, signOut, theme);
}

function brand(portal) {
  const title = (portal && portal.title) || meta('title') || 'API portal';
  document.getElementById('brand-title').textContent = title;
  document.title = title;
  const accent = (portal && portal.accent) || meta('accent') || 'rust';
  document.documentElement.setAttribute('data-accent', accent);
}

function footer(portal) {
  clear(footEl);
  if (portal && portal.contact) {
    const c = portal.contact;
    const link = /^https?:\/\//.test(c) ? h('a', { href: c, target: '_blank', rel: 'noopener noreferrer' }, c)
      : /^[^\s@]+@[^\s@]+$/.test(c) ? h('a', { href: 'mailto:' + c }, c) : h('span', null, c);
    footEl.appendChild(h('span', null, 'Questions: ', link));
  }
  footEl.appendChild(h('span', null, 'Mocked by Rustybin' + (portal && portal.version ? ' ' + portal.version : '')));
  if (portal && portal.base_url) footEl.appendChild(h('span', { class: 'mono' }, portal.base_url));
}

// ── Credentials and requests ───────────────────────────────────────

function credential() {
  if (AUTH === 'open') return null;
  try { return JSON.parse(load('session', CRED_KEY) || 'null'); } catch { return null; }
}

/** Headers for the portal's own API (api/*). */
function apiHeaders() {
  const c = credential();
  if (!c) return {};
  return c.kind === 'key' ? { [KEY_HEADER]: c.value } : { Authorization: 'Bearer ' + c.value };
}

/** Headers Try it adds to a call: the API key in key mode (never a control token). */
function tryHeaders() {
  const c = credential();
  return c && c.kind === 'key' ? { [KEY_HEADER]: c.value } : {};
}

async function api(path, opts = {}) {
  const headers = Object.assign({}, apiHeaders(), opts.headers || {});
  return fetch(path, Object.assign({}, opts, { headers, credentials: 'same-origin', cache: 'no-cache' }));
}

function report(item, event) {
  api('api/events', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ item, event }) }).catch(() => {});
}

const viewed = new Set();
function reportView(item) {
  if (viewed.has(item)) return;
  viewed.add(item);
  report(item, 'view');
}

async function loadCatalog() {
  const headers = etag && catalog ? { 'If-None-Match': etag } : {};
  const r = await api('api/catalog', { headers });
  if (r.status === 304 && catalog) return { ok: true };
  if (r.status === 401) {
    let body = {};
    try { body = await r.json(); } catch { body = {}; }
    return { ok: false, signin: true, body };
  }
  if (!r.ok) {
    let body = {};
    try { body = await r.json(); } catch { body = {}; }
    return { ok: false, status: r.status, body };
  }
  catalog = await r.json();
  etag = r.headers.get('ETag');
  brand(catalog.portal);
  footer(catalog.portal);
  return { ok: true };
}

// ── Sign-in ────────────────────────────────────────────────────────

function signinView(info) {
  const err = h('div');
  const isKey = AUTH === 'key';
  const user = input({ id: 'portal-user', autocomplete: 'username', 'aria-label': 'User name' });
  const pass = input({ id: 'portal-password', type: 'password', autocomplete: 'current-password', 'aria-label': 'Password' });
  const secret = input({ id: 'portal-secret', type: 'password', autocomplete: 'off', 'aria-label': isKey ? 'API key' : 'Token' });
  const usePassword = !isKey && SIGNIN;
  const fields = usePassword
    ? [field('User name', user), field('Password', pass)]
    : [field(isKey ? 'API key' : 'Control token', secret, isKey ? 'An API key of this instance (sent as ' + KEY_HEADER + ').' : 'The admin token or a control token.')];
  const form = h('form', { class: 'stack' }, ...fields, err,
    h('div', { class: 'row' }, h('span', { class: 'grow' }), h('button', { class: 'btn primary', type: 'submit', id: 'portal-sign-in' }, 'Open the portal')));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    replace(err);
    if (usePassword) {
      let r;
      try {
        r = await fetch('../_rustybin/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: user.value.trim(), password: pass.value }), credentials: 'same-origin' });
      } catch (ex) {
        replace(err, notice('err', 'The sign-in failed: ' + ex.message));
        return;
      }
      let body = {};
      try { body = await r.json(); } catch { body = {}; }
      if (!r.ok || !body.token) {
        replace(err, notice('err', body.error || ('The sign-in failed (' + r.status + ')')));
        return;
      }
      save('session', CRED_KEY, JSON.stringify({ kind: 'bearer', value: body.token }));
    } else {
      const v = secret.value.trim();
      if (!v) { replace(err, notice('err', isKey ? 'Enter an API key.' : 'Enter a token.')); return; }
      save('session', CRED_KEY, JSON.stringify({ kind: isKey ? 'key' : 'bearer', value: v }));
    }
    const res = await loadCatalog();
    if (!res.ok) {
      save('session', CRED_KEY, null);
      replace(err, notice('err', res.signin ? (isKey ? 'This key was not accepted.' : 'These credentials were not accepted.') : ('The portal answered ' + res.status)));
      return;
    }
    topActions();
    route();
  });
  return h('div', { class: 'signin-portal' }, card(meta('title') || 'API portal', { hint: isKey ? 'API key required' : 'sign-in required' },
    h('p', { class: 'muted small', style: { marginTop: 0 } }, isKey
      ? 'This portal is for holders of an API key of this instance.'
      : usePassword ? 'Sign in with the user name and password of this instance.' : 'This portal needs a control-plane token of this instance.'),
    info && info.error ? null : null,
    form));
}

// ── Building blocks ────────────────────────────────────────────────

function kindBadge(kind) {
  return h('span', { class: 'badge info' }, icon(KIND_ICONS[kind] || 'file'), KIND_LABELS[kind] || kind);
}

function itemBadges(it) {
  const out = [kindBadge(it.kind)];
  if (it.new) out.push(badge('new', 'ok'));
  if (it.visitor) out.push(badge('visitor', 'warn'));
  if (it.kind === 'api' && it.detail && it.detail.state === 'off') out.push(badge('switched off', 'err'));
  for (const t of it.tags || []) out.push(h('span', { class: 'tag' }, t));
  return out;
}

function itemHref(id) {
  return '#/i/' + encodeURIComponent(id);
}

function itemCard(it) {
  return h('a', { class: 'item-card', href: itemHref(it.id), 'data-item': it.id },
    h('h3', null, it.title),
    it.summary ? h('div', { class: 'summary' }, it.summary.replace(/[#*`_]/g, '').split('\n')[0]) : null,
    h('div', { class: 'badges' }, ...itemBadges(it)));
}

function matches(it, q) {
  if (!q) return true;
  const hay = [it.title, it.summary, it.id, ...(it.tags || [])].join(' ').toLowerCase();
  return q.toLowerCase().split(/\s+/).filter(Boolean).every((w) => hay.includes(w));
}

function snippet(label, text, lang) {
  return h('div', null, h('div', { class: 'snippet-label' }, label), copyableCode(text, lang || null, 'short'));
}

function headerLines(list) {
  return (list || []).map((x) => x.name + ': ' + x.value).join('\n');
}

function parseHeaderLines(text) {
  const out = [];
  for (const line of String(text || '').split('\n')) {
    const i = line.indexOf(':');
    if (i <= 0) continue;
    const name = line.slice(0, i).trim();
    if (name) out.push([name, line.slice(i + 1).trim()]);
  }
  return out;
}

/**
 * Try it: an editable request sent from this browser to `base + path`.
 * opts: {item, method, base, path, headers: [{name, value}], body}
 */
function tryPanel(opts) {
  const methodSel = select(['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'], (opts.method || 'GET').toUpperCase(), { 'aria-label': 'Method' });
  const url = input({ value: opts.base + (opts.path || ''), 'aria-label': 'URL', class: 'input mono try-url' });
  const hdrs = textarea({ rows: Math.max(2, (opts.headers || []).length + 1), 'aria-label': 'Headers', placeholder: 'Name: value, one per line' });
  hdrs.value = headerLines(opts.headers);
  const body = textarea({ rows: opts.body ? Math.min(14, String(opts.body).split('\n').length + 1) : 3, 'aria-label': 'Body', placeholder: 'Request body' });
  body.value = opts.body || '';
  const out = h('div', { class: 'try-result', 'aria-live': 'polite' });
  const sendBtn = h('button', { class: 'btn primary', type: 'submit' }, icon('send'), 'Send');
  const form = h('form', { class: 'try' },
    h('div', { class: 'url-line' }, methodSel, url, sendBtn),
    h('details', { class: 'disclosure' }, h('summary', null, 'Headers and body'),
      h('div', { class: 'stack-sm', style: { paddingTop: '8px' } }, field('Headers', hdrs), field('Body', body))),
    out);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const method = methodSel.value;
    const headers = new Headers();
    for (const [k, v] of parseHeaderLines(hdrs.value)) {
      try { headers.append(k, v); } catch { /* invalid header name */ }
    }
    for (const [k, v] of Object.entries(tryHeaders())) if (!headers.has(k)) headers.set(k, v);
    const init = { method, headers, credentials: 'omit' };
    if (!['GET', 'HEAD'].includes(method) && body.value !== '') init.body = body.value;
    sendBtn.disabled = true;
    replace(out, spinner('Sending'));
    const started = performance.now();
    try {
      const r = await fetch(url.value.trim(), init);
      const text = await r.text();
      const ms = performance.now() - started;
      const rows = [];
      r.headers.forEach((value, name) => rows.push({ name, value }));
      replace(out,
        h('div', { class: 'row' }, statusBadge(r.status, r.statusText), h('span', { class: 'muted small' }, fmtMs(ms))),
        h('details', { class: 'disclosure' }, h('summary', null, 'Response headers (' + rows.length + ')'), headersTable(rows, { noHighlight: true })),
        bodyView(text, r.headers.get('content-type') || ''));
      report(opts.item, 'try');
    } catch (ex) {
      replace(out, notice('err', 'The request failed: ' + (ex && ex.message ? ex.message : ex) + '. ',
        catalog && catalog.portal && catalog.portal.gateway ? 'Requests go through the gateway URL; it needs CORS for this page.' : ''));
    } finally {
      sendBtn.disabled = false;
    }
  });
  return form;
}

// ── Home ───────────────────────────────────────────────────────────

function homeView(query) {
  const p = catalog.portal || {};
  const items = catalog.items || [];
  const el = h('div');
  const intro = p.intro ? renderMarkdown(p.intro)
    : h('div', { class: 'md' }, h('p', null, 'Mock APIs, MCP servers, an LLM and agents to build and test against. Open one to read its reference, try a request and copy the snippets.'));
  el.appendChild(h('section', { class: 'hero' }, h('h1', null, p.title || 'API portal'), intro,
    h('div', { class: 'kind-chips' }, ...(catalog.kinds || []).map((k) =>
      h('a', { class: 'kind-chip', href: '#/k/' + k.id }, icon(KIND_ICONS[k.id] || 'file'), k.title, h('span', { class: 'count' }, String(k.count)))))));
  if (!items.length) {
    el.appendChild(empty('Nothing has been published yet', 'The operator of this instance publishes APIs, MCP servers and agents from the console.', 'inbox'));
    return el;
  }
  const fresh = (catalog.whats_new || []).map((id) => items.find((i) => i.id === id)).filter(Boolean);
  if (fresh.length && !query) {
    el.appendChild(h('div', { class: 'whats-new', role: 'region', 'aria-label': 'What is new' },
      h('strong', null, 'New this week:'),
      ...fresh.map((it) => h('a', { href: itemHref(it.id) }, it.title))));
  }
  const shown = items.filter((it) => matches(it, query));
  if (query) el.appendChild(h('p', { class: 'muted' }, shown.length + ' result' + (shown.length === 1 ? '' : 's') + ' for "' + query + '"'));
  for (const k of catalog.kinds || []) {
    const list = shown.filter((it) => it.kind === k.id);
    if (!list.length) continue;
    el.appendChild(h('section', { class: 'kind-section', id: 'kind-' + k.id },
      h('h2', null, icon(KIND_ICONS[k.id] || 'file'), k.title),
      h('div', { class: 'item-grid' }, ...list.map(itemCard))));
  }
  if (query && !shown.length) el.appendChild(empty('No match', 'Try another word.', 'explorer'));
  return el;
}

function kindView(kind) {
  const k = (catalog.kinds || []).find((x) => x.id === kind);
  const list = (catalog.items || []).filter((it) => it.kind === kind);
  return h('div', null,
    h('div', { class: 'crumbs' }, h('a', { href: '#/' }, 'Home'), '/', h('span', null, k ? k.title : kind)),
    h('h1', null, k ? k.title : kind),
    list.length ? h('div', { class: 'item-grid' }, ...list.map(itemCard)) : empty('Nothing here', null, 'inbox'));
}

// ── Item pages ─────────────────────────────────────────────────────

function itemView(id) {
  const it = (catalog.items || []).find((x) => x.id === id);
  if (!it) return h('div', null, empty('Not found', 'This item is not published (any more).', 'alert'), h('p', null, h('a', { href: '#/' }, 'Back to the portal')));
  reportView(it.id);
  const kindTitle = ((catalog.kinds || []).find((k) => k.id === it.kind) || {}).title || it.kind;
  const head = h('div', null,
    h('div', { class: 'crumbs' }, h('a', { href: '#/' }, 'Home'), '/', h('a', { href: '#/k/' + it.kind }, kindTitle), '/', h('span', null, it.title)),
    h('div', { class: 'item-head' },
      h('div', { class: 'grow' }, h('h1', null, it.title),
        h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '6px', marginTop: '8px' } }, ...itemBadges(it)),
        it.summary ? renderMarkdown(it.summary) : null,
        it.contact ? h('p', { class: 'small muted' }, 'Contact: ', it.contact) : null)));
  const body = it.kind === 'api' ? apiPage(it)
    : it.kind === 'mcp' ? mcpPage(it)
      : it.kind === 'llm' ? llmPage(it)
        : it.kind === 'agent' ? agentPage(it)
          : it.kind === 'tools' ? toolsPage(it)
            : it.kind === 'registry' ? registryPage(it) : h('div');
  return h('div', { class: 'stack' }, head, body);
}

function securityCard(d) {
  const s = d.security || {};
  if (!s.enabled) return card('Authentication', {}, h('p', { class: 'small', style: { margin: 0 } }, 'None: every operation answers without credentials.'));
  const rows = [];
  for (const sc of s.schemes || []) {
    rows.push(h('li', null, h('strong', null, sc.name || sc.type), sc.hint ? ': ' + sc.hint : ''));
  }
  const creds = [];
  if (d.credentials) {
    const keys = (s.api_key && s.api_key.keys) || [];
    if ((s.mode === 'api_key' || s.mode === 'spec') && keys.length) creds.push(['API keys', h('span', { class: 'mono' }, keys.join(', '))]);
    if (s.mode === 'api_key' && s.api_key) creds.push(['API key header', h('span', { class: 'mono' }, s.api_key.header)]);
    const users = (s.basic && s.basic.users) || [];
    if ((s.mode === 'basic' || s.mode === 'spec') && users.length) creds.push(['Basic users', h('span', { class: 'mono' }, users.join(', '))]);
    const tokens = (s.bearer && s.bearer.tokens) || [];
    if ((s.mode === 'bearer' || s.mode === 'spec') && tokens.length) creds.push(['Bearer tokens', h('span', { class: 'mono' }, tokens.join(', '))]);
  }
  creds.push(['JWTs', h('span', null, 'from the built-in identity provider: ', h('span', { class: 'mono' }, 'POST ' + d.token_url))]);
  return card('Authentication', { hint: 'mocked: demo credentials only' },
    h('div', { class: 'stack-sm' },
      rows.length ? h('ul', { style: { margin: 0, paddingLeft: '18px' } }, ...rows) : h('p', { class: 'small', style: { margin: 0 } }, 'Mode: ' + s.mode),
      kvgrid(creds),
      d.credentials ? h('p', { class: 'muted small', style: { margin: 0 } }, 'The examples below already carry working demo credentials.')
        : h('p', { class: 'muted small', style: { margin: 0 } }, 'The operator keeps the demo credentials private: ask them for a key.')));
}

async function download(it, fmt, file, btn) {
  btn.disabled = true;
  try {
    const r = await api('api/items/' + encodeURIComponent(it.id) + '/collections/' + fmt);
    if (!r.ok) { toast('The download failed (' + r.status + ')', 'err'); return; }
    const blob = await r.blob();
    const a = h('a', { href: URL.createObjectURL(blob), download: file });
    document.body.appendChild(a);
    a.click();
    setTimeout(() => { URL.revokeObjectURL(a.href); a.remove(); }, 1000);
  } catch (ex) {
    toast('The download failed: ' + ex.message, 'err');
  } finally {
    btn.disabled = false;
  }
}

function operationsCard(it) {
  const d = it.detail;
  const groups = new Map();
  for (const op of d.operations || []) {
    const tag = (op.tags && op.tags[0]) || 'Operations';
    if (!groups.has(tag)) groups.set(tag, []);
    groups.get(tag).push(op);
  }
  const body = h('div', { class: 'stack' });
  for (const [tag, ops] of groups) {
    body.appendChild(h('div', null, h('h3', { class: 'section-title' }, tag),
      ...ops.map((op) => {
        const det = h('details', { class: 'op', 'data-op': op.id },
          h('summary', null, methodBadge(op.method), h('span', { class: 'path' }, op.path),
            op.summary ? h('span', { class: 'sum' }, op.summary) : null,
            op.deprecated ? badge('deprecated', 'warn') : null,
            op.secured ? h('span', { title: 'Needs credentials' }, icon('lock')) : null,
            op.has_example ? null : badge('generated data', '')));
        let built = false;
        det.addEventListener('toggle', () => {
          if (!det.open || built) return;
          built = true;
          det.appendChild(h('div', { class: 'op-body' },
            op.description ? renderMarkdown(op.description) : null,
            tryPanel({ item: it.id, method: op.sample.method, base: d.base_url, path: op.sample.path, headers: op.sample.headers, body: op.sample.body }),
            snippet('curl', op.curl)));
        });
        return det;
      })));
  }
  return card('Operations', { hint: (d.operations || []).length + ' operations, ' + d.coverage.with_examples + ' with examples' }, body);
}

function apiPage(it) {
  const d = it.detail;
  const el = h('div', { class: 'stack' });
  el.appendChild(card('Overview', { actions: h('a', { class: 'btn sm', href: d.reference_path.replace(/^\/portal\//, ''), target: '_blank', rel: 'noopener' }, icon('external'), 'Full API reference') },
    kvgrid([
      ['Base URL', h('span', { class: 'row nowrap' }, h('span', { class: 'mono break' }, d.base_url), copyButton(d.base_url, { icon: true, title: 'Copy the base URL' }))],
      ['API', d.api_title + ' ' + d.version],
      ['Spec', h('span', { class: 'row', style: { gap: '10px' } }, h('a', { href: d.spec_url, target: '_blank', rel: 'noopener' }, 'OpenAPI JSON'), h('a', { href: d.spec_yaml_url, target: '_blank', rel: 'noopener' }, 'YAML'))],
      ['Examples', d.coverage.with_examples + ' of ' + d.coverage.operations + ' operations have examples; the others answer generated data'],
      ['State', d.state === 'live' ? 'live' : h('span', null, badge(d.state, 'warn'), ' the mock answers 503 while it is switched off')],
    ]),
    d.description && d.description !== it.summary ? renderMarkdown(d.description) : null));
  el.appendChild(securityCard(d));
  el.appendChild(operationsCard(it));
  el.appendChild(card('Downloads', { hint: 'collections with every operation' },
    h('div', { class: 'downloads' }, ...(d.downloads || []).map((x) => {
      const b = h('button', { class: 'btn sm', type: 'button', 'data-format': x.format }, icon('download'), FORMAT_LABELS[x.format] || x.format);
      b.addEventListener('click', () => download(it, x.format, x.file, b));
      return b;
    }))));
  if (d.mcp) {
    el.appendChild(card('MCP server', { hint: d.mcp.tools.length + ' tools, one per operation' },
      h('div', { class: 'stack-sm' },
        kvgrid([['URL', h('span', { class: 'mono break' }, d.mcp.url)]]),
        snippet('Claude Code', d.mcp.claude),
        snippet('mcpServers JSON', JSON.stringify(d.mcp.json, null, 2), 'json'),
        toolTable(d.mcp.tools.map((t) => ({ name: t.name, description: t.description, extra: t.method + ' ' + t.path }))))));
  }
  if (d.registry) {
    el.appendChild(card('MCP registry', {}, h('p', { class: 'small', style: { margin: 0 } }, 'This server is listed in the mock MCP registry: ', h('a', { href: d.registry.search_url, target: '_blank', rel: 'noopener' }, d.registry.search_url))));
  }
  return el;
}

function toolTable(tools) {
  if (!tools.length) return h('p', { class: 'muted small' }, 'No tools.');
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
    h('thead', null, h('tr', null, h('th', null, 'Tool'), h('th', null, 'What it does'))),
    h('tbody', null, ...tools.map((t) => h('tr', null,
      h('td', { class: 'mono small' }, t.name, t.extra ? h('div', { class: 'muted' }, t.extra) : null),
      h('td', { class: 'small' }, t.description || ''))))));
}

function mcpPage(it) {
  const d = it.detail;
  const auth = d.auth || {};
  const tools = (d.tools || []).map((t) => ({ name: t.name, description: t.description }));
  const schemaList = h('div', { class: 'stack-sm' }, ...(d.tools || []).map((t) => h('details', { class: 'op' },
    h('summary', null, h('span', { class: 'path' }, t.name), h('span', { class: 'sum' }, t.title || '')),
    h('div', { class: 'op-body' }, t.description ? h('p', { class: 'small', style: { margin: 0 } }, t.description) : null,
      codeBlock(JSON.stringify(t.inputSchema || {}, null, 2), 'json', 'short')))));
  return h('div', { class: 'stack' },
    card('Connect', { hint: d.transport === 'sse' ? 'legacy HTTP+SSE transport' : 'Streamable HTTP' },
      h('div', { class: 'stack-sm' },
        kvgrid([
          ['URL', h('span', { class: 'row nowrap' }, h('span', { class: 'mono break' }, d.url), copyButton(d.url, { icon: true, title: 'Copy the URL' }))],
          ['Authentication', auth.kind === 'none' ? 'none' : h('span', null, h('strong', null, auth.kind === 'oauth' ? 'OAuth 2.1' : 'API key'), auth.hint ? ': ' + auth.hint : '')],
        ]),
        snippet('Claude Code', d.claude),
        snippet('mcpServers JSON', JSON.stringify(d.json, null, 2), 'json'),
        h('p', { class: 'muted small', style: { margin: 0 } }, 'Test it with the MCP inspector of the console, if you have access: ', h('a', { href: '..' + d.inspector, target: '_blank', rel: 'noopener' }, 'open the inspector')))),
    card('Tools', { hint: tools.length + ' tools' }, toolTable(tools)),
    tools.length ? card('Input schemas', {}, schemaList) : null);
}

function llmPage(it) {
  const d = it.detail;
  const base = (catalog.portal && catalog.portal.base_url) || '';
  return h('div', { class: 'stack' },
    card('Providers', { hint: d.note }, h('div', { class: 'stack' }, ...(d.providers || []).map((p) => h('details', { class: 'op', 'data-provider': p.id },
      h('summary', null, h('strong', null, p.title), h('span', { class: 'path' }, p.base_url)),
      h('div', { class: 'op-body' },
        h('div', { class: 'small' }, 'Routes below the base URL: ', ...p.routes.map((r, i) => h('span', null, i ? ', ' : '', h('code', null, r)))),
        tryPanel({ item: it.id, method: 'POST', base, path: p.sample.path, headers: p.sample.headers, body: p.sample.body }),
        snippet('curl', p.curl)))))),
    card('Shaping the answers', { hint: 'request headers' }, h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
      h('thead', null, h('tr', null, h('th', null, 'Header'), h('th', null, 'Effect'))),
      h('tbody', null, ...(d.headers || []).map((x) => h('tr', null, h('td', { class: 'mono small' }, x.name), h('td', { class: 'small' }, x.effect))))))));
}

function agentPage(it) {
  const d = it.detail;
  const base = (catalog.portal && catalog.portal.base_url) || '';
  return h('div', { class: 'stack' },
    card('Agent', { hint: 'A2A protocol' }, kvgrid([
      ['URL', h('span', { class: 'mono break' }, d.url)],
      ['Agent card', h('a', { href: d.card_url, target: '_blank', rel: 'noopener' }, d.card_url)],
      ['Authentication', d.secured ? 'a bearer token from the built-in identity provider' : 'none'],
      ['Input', (d.input_modes || []).join(', ')],
      ['Output', (d.output_modes || []).join(', ')],
    ])),
    card('Skills', {}, h('div', { class: 'stack-sm' }, ...(d.skills || []).map((s) => h('div', null,
      h('strong', null, s.name), h('span', { class: 'muted small' }, ' (' + s.id + ')'),
      h('div', { class: 'small' }, s.description),
      s.examples && s.examples.length ? h('div', { class: 'small muted' }, 'Try: ', s.examples.map((x) => '"' + x + '"').join(', ')) : null)))),
    card('Send a message', {}, h('div', { class: 'stack-sm' },
      tryPanel({ item: it.id, method: 'POST', base, path: d.sample.path, headers: d.sample.headers, body: d.sample.body }),
      snippet('curl', d.curl))));
}

function toolsPage(it) {
  const d = it.detail;
  const base = (catalog.portal && catalog.portal.base_url) || '';
  return h('div', { class: 'stack' }, ...(d.categories || []).map((c) => card(c.name, { hint: c.endpoints.length + ' routes' },
    h('div', null, ...c.endpoints.map((ep) => {
      const det = h('details', { class: 'op' },
        h('summary', null, ...(ep.methods || []).slice(0, 3).map(methodBadge), h('span', { class: 'path' }, ep.path), h('span', { class: 'sum' }, ep.summary),
          ep.protocol !== 'http' ? badge(ep.protocol, 'info') : null));
      let built = false;
      det.addEventListener('toggle', () => {
        if (!det.open || built) return;
        built = true;
        const ex = (ep.examples || [])[0];
        det.appendChild(h('div', { class: 'op-body' },
          ep.description ? h('p', { class: 'small', style: { margin: 0 } }, ep.description) : null,
          ex && ep.protocol === 'http'
            ? tryPanel({ item: it.id, method: ex.method, base, path: ex.path, headers: (ex.headers || []).concat(ex.auth && ex.auth.type === 'bearer' ? [{ name: 'Authorization', value: 'Bearer ' + ex.auth.token }] : []), body: ex.body || '' })
            : h('p', { class: 'muted small', style: { margin: 0 } }, ep.protocol === 'websocket' ? 'A WebSocket: connect with a WebSocket client.' : 'Open it with a client that reads ' + ep.protocol + '.'),
          (ep.examples || []).length > 1 ? h('div', { class: 'small muted' }, 'More examples: ', ep.examples.slice(1).map((x) => x.method + ' ' + x.path).join(', ')) : null));
      });
      return det;
    })))));
}

function registryPage(it) {
  const d = it.detail;
  const base = (catalog.portal && catalog.portal.base_url) || '';
  return h('div', { class: 'stack' },
    card('Registry', { hint: 'the registry API /v0.1' }, h('div', { class: 'stack-sm' },
      kvgrid([
        ['Base URL', h('span', { class: 'mono break' }, d.url)],
        ['Servers', h('a', { href: d.servers_url, target: '_blank', rel: 'noopener' }, d.servers_url)],
        ['Scenario', d.scenario],
      ]),
      tryPanel({ item: it.id, method: 'GET', base, path: d.servers_url.slice(base.length) + '?limit=10', headers: [], body: '' }),
      snippet('curl', d.curl))));
}

// ── Router ─────────────────────────────────────────────────────────

let routing = 0;
async function route() {
  const token = ++routing;
  const hash = location.hash.replace(/^#\/?/, '');
  const [section, ...rest] = hash.split('/');
  const arg = decodeURIComponent(rest.join('/'));
  if (!catalog) {
    replace(viewEl, spinner('Loading the portal'));
    let res;
    try {
      res = await loadCatalog();
    } catch (ex) {
      res = { ok: false, status: 0, body: { error: ex.message } };
    }
    if (token !== routing) return;
    if (!res.ok) {
      if (res.signin) { replace(viewEl, signinView(res.body)); return; }
      replace(viewEl, notice('err', (res.body && res.body.error) || ('The portal answered ' + res.status), res.body && res.body.hint ? ' ' + res.body.hint : ''));
      return;
    }
  } else {
    loadCatalog().catch(() => {});
  }
  if (token !== routing) return;
  const q = searchEl.value.trim();
  let view;
  if (section === 'i' && arg) view = itemView(arg);
  else if (section === 'k' && arg) view = kindView(arg);
  else view = homeView(q);
  replace(viewEl, view);
  if (section === 'i' || section === 'k') window.scrollTo(0, 0);
}

applyTheme();
brand(null);
topActions();
footer(null);
document.getElementById('search-form').addEventListener('submit', (e) => {
  e.preventDefault();
  if (location.hash && location.hash !== '#/') location.hash = '#/';
  else route();
});
searchEl.addEventListener('input', () => {
  if (!catalog) return;
  const hash = location.hash.replace(/^#\/?/, '');
  if (hash && hash !== '') { location.hash = '#/'; return; }
  replace(viewEl, homeView(searchEl.value.trim()));
});
window.addEventListener('hashchange', route);
route();
