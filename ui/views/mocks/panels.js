// Tabs of a mock's detail view that talk to the mock itself: Try it (send a
// prefilled request), Security (switch, credentials, tokens, per-operation
// access) and Traffic (live captured calls over fetch-based SSE).
import { h, replace, clear, card, icon, notice, spinner, empty, methodBadge, badge, statusBadge, input, select, textarea, toast, copyButton, toggleSwitch, tabs, kvgrid, fmtTime, fmtBytes, fmtMs } from '../../lib/dom.js';
import { bodyView, headersTable, codeBlock, copyableCode } from '../../lib/format.js';
import { SERVER, send, toCurl, readText, sseParser } from '../../lib/http.js';
import { responseView, headerEditor } from '../../lib/widgets.js';
import { api, errorText, report, groupByTag, sampleToRequest, relTime } from '../../lib/mockapi.js';
import { issuesList } from './operation.js';

const enc = encodeURIComponent;

/** POST /oauth/token (client_credentials with the demo client) -> token or throws. */
export async function idpToken() {
  const r = await send({ method: 'POST', url: SERVER + '/oauth/token', headers: [['Content-Type', 'application/x-www-form-urlencoded']],
    body: 'grant_type=client_credentials&client_id=rustybin&client_secret=secret', session: false, timeout: 15000 });
  if (!r.ok || !r.json || !r.json.access_token) throw new Error(r.error || (r.json && (r.json.error_description || r.json.error)) || `The token endpoint answered ${r.status}`);
  return r.json.access_token;
}

function opOptions(sel, ops) {
  clear(sel);
  for (const [tag, list] of groupByTag(ops)) {
    const g = h('optgroup', { label: tag });
    for (const op of list) g.appendChild(h('option', { value: op.id }, `${op.method} ${op.path}${op.summary ? '  (' + op.summary + ')' : ''}`));
    sel.appendChild(g);
  }
}

// ── Try it ─────────────────────────────────────────────────────────

/** ctx: {name, detail(), goTab(id)}. Returns {el, update(detail), select(opId)}. */
export function tryPanel(ctx) {
  const name = ctx.name;
  const cache = new Map();
  let current = null;
  const opSel = h('select', { class: 'select', 'aria-label': 'Operation' });
  const methodSel = select(['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'], 'GET', { 'aria-label': 'Method', style: { width: '110px' } });
  const baseEl = h('span', { class: 'mono small muted try-base' });
  const pathIn = input({ class: 'input mono', 'aria-label': 'Path and query (below the mock URL)' });
  const hdrs = headerEditor([]);
  const bodyIn = textarea({ rows: 7, placeholder: 'Request body', 'aria-label': 'Request body' });
  const preferSel = h('select', { class: 'select', 'aria-label': 'Prefer a response' });
  const sendBtn = h('button', { class: 'btn primary', type: 'button', title: 'Send (Ctrl+Enter)' }, icon('send'), 'Send');
  const respEl = h('div');
  const bodyDetails = h('details', { class: 'disclosure' }, h('summary', null, 'Body'), bodyIn);
  const tokenBtn = h('button', { class: 'btn sm', type: 'button', hidden: true }, icon('token'), 'Add a token from the built-in IdP');

  function request() {
    const p = pathIn.value.trim() || '/';
    const body = bodyIn.value;
    return { method: methodSel.value, url: SERVER + '/mock/' + name + (p.startsWith('/') ? p : '/' + p), headers: hdrs.get(), body: body && methodSel.value !== 'GET' && methodSel.value !== 'HEAD' ? body : undefined };
  }

  function setHeader(key, value) {
    const list = hdrs.get().filter(([k]) => k.toLowerCase() !== key.toLowerCase());
    if (value) list.push([key, value]);
    hdrs.set(list);
  }

  async function selectOp(id) {
    if (!id) return;
    opSel.value = id;
    replace(respEl);
    let d = cache.get(id);
    if (!d) {
      const r = await api('GET', `/${enc(name)}/operations/${enc(id)}`);
      if (!r.ok || !r.json) { replace(respEl, notice('err', errorText(r, 'Loading the operation'))); return; }
      d = r.json;
      cache.set(id, d);
    }
    current = d;
    const req = sampleToRequest(name, d.sample_request);
    methodSel.value = req.method;
    pathIn.value = req.path;
    hdrs.set(req.headers);
    bodyIn.value = req.body || '';
    bodyDetails.open = !!req.body;
    clear(preferSel);
    preferSel.appendChild(h('option', { value: '' }, 'Prefer: none (the mock decides)'));
    for (const resp of d.operation.responses || []) {
      if (/^\d{3}$/.test(resp.status)) preferSel.appendChild(h('option', { value: 'code=' + resp.status }, 'Status ' + resp.status + (resp.description ? ': ' + resp.description : '')));
      for (const m of resp.content || []) for (const ex of m.examples || []) preferSel.appendChild(h('option', { value: 'example=' + ex.name }, 'Example ' + ex.name + ' (' + resp.status + ')'));
    }
    preferSel.appendChild(h('option', { value: 'dynamic=true' }, 'Generated data (dynamic=true)'));
    const det = ctx.detail();
    tokenBtn.hidden = !(det && det.security && det.security.enabled && ['spec', 'bearer', 'jwt'].includes(det.security.mode));
  }

  preferSel.addEventListener('change', () => setHeader('Prefer', preferSel.value));
  opSel.addEventListener('change', () => selectOp(opSel.value));
  tokenBtn.addEventListener('click', async () => {
    try { setHeader('Authorization', 'Bearer ' + await idpToken()); toast('Token added (Authorization header)'); } catch (e) { toast(e.message, 'err'); }
  });

  async function doSend() {
    const req = request();
    sendBtn.disabled = true;
    replace(respEl, card(null, null, spinner('Sending ' + req.method + ' ' + req.url)));
    const r = await send(Object.assign({ timeout: 60000 }, req));
    sendBtn.disabled = false;
    if (!respEl.isConnected) return;
    const opHeader = r.header('x-rustybin-mock-operation');
    const src = r.header('x-rustybin-mock-source');
    const failed = r.header('x-rustybin-mock-validation') === 'failed';
    const det = ctx.detail();
    const validating = det && det.validate_requests !== 'off';
    replace(respEl, h('div', { class: 'stack-sm' },
      r.error ? null : h('div', { class: 'row mock-strip' },
        opHeader ? badge(h('span', null, 'operation ', h('strong', null, opHeader)), 'outline') : badge('no operation matched', 'warn'),
        src ? badge('source ' + src, 'info') : null,
        opHeader && validating ? (failed ? badge(h('span', null, icon('alert'), ' request does not match the spec'), 'warn') : badge(h('span', null, icon('check'), ' request matches the spec'), 'ok')) : null,
        failed || r.status === 401 || r.status === 403 || r.status === 400 ? h('button', { class: 'linklike small', type: 'button', onclick: () => ctx.goTab('traffic') }, 'Details in Traffic') : null),
      responseView(r)));
    respEl.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    if (ctx.refreshHead) ctx.refreshHead();
  }
  sendBtn.addEventListener('click', doSend);
  const el = h('div', { class: 'stack' },
    card('Try it', { hint: 'requests go to the deployed revision' }, h('div', { class: 'stack-sm' },
      h('label', { class: 'field' }, h('span', null, 'Operation'), opSel),
      h('div', { class: 'row nowrap' }, methodSel, baseEl),
      h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, pathIn), sendBtn,
        copyButton(() => toCurl(request()), { label: 'Copy as curl', toast: 'curl command copied' })),
      h('div', { class: 'grid grid-2' }, h('label', { class: 'field' }, h('span', null, 'Pick a response (Prefer header)'), preferSel), h('div', { class: 'field' }, h('span', null, 'Credentials'), h('div', null, tokenBtn), h('span', { class: 'hint' }, 'Demo credentials are prefilled when mocked security is on.'))),
      h('details', { class: 'disclosure', open: true }, h('summary', null, 'Headers'), hdrs.el),
      bodyDetails,
      h('p', { class: 'tiny muted', style: { margin: 0 } }, 'Ctrl+Enter sends. The request is sent from this browser, exactly like the curl command.'))),
    respEl);
  el.addEventListener('keydown', (e) => { if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); doSend(); } });

  let seenRevision = null;
  function update(detail) {
    const before = opSel.value;
    opOptions(opSel, detail.operations || []);
    baseEl.textContent = SERVER + '/mock/' + name;
    // A new draft revision may change the sample (credentials, parameters).
    const changed = seenRevision !== detail.draft_revision;
    seenRevision = detail.draft_revision;
    if (changed) cache.clear();
    const keep = (detail.operations || []).some((o) => o.id === before) ? before : (detail.operations[0] && detail.operations[0].id);
    if (keep && (changed || !current || keep !== current.operation.id)) selectOp(keep);
    else if (keep) opSel.value = keep;
  }
  return { el, update, select: selectOp };
}

// ── Security ───────────────────────────────────────────────────────

const MODES = [
  ['spec', "The spec's schemes", 'Each operation needs the credentials of the schemes its spec declares (API key, basic, bearer, OAuth2 and OpenID Connect JWTs from the built-in IdP).'],
  ['api_key', 'API key', 'Every operation needs one API key header.'],
  ['basic', 'Basic auth', 'Every operation needs a configured user and password.'],
  ['bearer', 'Bearer token', 'Every operation needs a configured token or a JWT from the built-in IdP.'],
  ['jwt', 'JWT only', 'Every operation needs a valid JWT from the built-in IdP.'],
];

/** Editable list of strings. */
function listEditor(values, placeholder, label) {
  const rows = h('div', { class: 'stack-sm' });
  function add(v) {
    const inp = h('input', { class: 'input sm mono', value: v, placeholder, 'aria-label': label });
    const row = h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, inp), h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove', onclick: () => row.remove() }, icon('x')));
    rows.appendChild(row);
  }
  for (const v of values || []) add(v);
  return {
    el: h('div', { class: 'stack-sm' }, rows, h('div', null, h('button', { class: 'btn sm', type: 'button', onclick: () => add('') }, icon('plus'), 'Add'))),
    get: () => Array.from(rows.querySelectorAll('input')).map((i) => i.value.trim()).filter(Boolean),
  };
}

function usersEditor(users) {
  const rows = h('div', { class: 'stack-sm' });
  function add(u, p) {
    const uIn = h('input', { class: 'input sm mono', value: u, placeholder: 'user', 'aria-label': 'User name' });
    const pIn = h('input', { class: 'input sm mono', value: p, placeholder: 'password', 'aria-label': 'Password' });
    const row = h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, uIn), h('div', { class: 'grow' }, pIn),
      h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove user', onclick: () => row.remove() }, icon('x')));
    rows.appendChild(row);
  }
  for (const [u, p] of Object.entries(users || {})) add(u, p);
  return {
    el: h('div', { class: 'stack-sm' }, rows, h('div', null, h('button', { class: 'btn sm', type: 'button', onclick: () => add('', '') }, icon('plus'), 'Add user'))),
    get() {
      const out = {};
      for (const row of rows.children) {
        const [u, p] = row.querySelectorAll('input');
        if (u.value.trim()) out[u.value.trim()] = p.value;
      }
      return out;
    },
  };
}

/** Example header lines for a scheme of credentials.schemes. */
function useLines(scheme, sec) {
  const key = (sec.api_key.keys || [])[0] || '<key>';
  const user = Object.entries(sec.basic.users || {})[0];
  const basic = user ? 'Authorization: Basic ' + btoa(user[0] + ':' + user[1]) : null;
  const bearer = (sec.bearer.tokens || [])[0];
  switch (scheme.type) {
    case 'api_key':
      if (scheme.in === 'query') return ['?' + scheme.name + '=' + key];
      if (scheme.in === 'cookie') return ['Cookie: ' + scheme.name + '=' + key];
      return [(scheme.in ? scheme.name : sec.api_key.header) + ': ' + key];
    case 'basic': return basic ? [basic] : [];
    case 'bearer': return bearer ? ['Authorization: Bearer ' + bearer] : [];
    default: return [];
  }
}

/** ctx: {name, detail(), refresh(), readOnly, goTab(id)}. */
export function securityPanel(ctx) {
  const el = h('div', { class: 'stack' });
  const tokenEl = h('div');
  let dirty = false;
  let last = null;

  async function getToken() {
    replace(tokenEl, spinner('Asking the built-in IdP'));
    try {
      const t = await idpToken();
      replace(tokenEl, h('div', { class: 'stack-sm' }, h('div', { class: 'token-box' }, t),
        h('div', { class: 'row' }, copyButton('Authorization: Bearer ' + t, { label: 'Copy header', toast: 'Header copied' }), copyButton(t, { label: 'Copy token', toast: 'Token copied' })),
        h('p', { class: 'tiny muted', style: { margin: 0 } }, 'client_credentials grant of the demo client (rustybin / secret) at POST /oauth/token. Valid for an hour.')));
    } catch (e) {
      replace(tokenEl, notice('err', e.message));
    }
  }

  async function setEnabled(on, sw) {
    const d = ctx.detail();
    let deploy = true;
    if (d.has_draft_changes) {
      deploy = confirm(`The draft has other changes that are not live yet. Deploy them together with security ${on ? 'on' : 'off'}?\n\nOK deploys everything, Cancel only changes the draft.`);
    }
    const sec = Object.assign({}, d.settings.security, { enabled: on });
    const r = await api('PATCH', '/' + enc(ctx.name), { security: sec, deploy });
    if (r.ok) toast(deploy ? `Security ${on ? 'on' : 'off'}` : `Security ${on ? 'on' : 'off'} in the draft: deploy to publish it`);
    else if (r.status === 422) toast('Saved to the draft, but the deploy checks failed: see the Deploy report tab.', 'err');
    else { toast(errorText(r, 'Changing security'), 'err'); sw.set(!on); return; }
    dirty = false;
    ctx.refresh();
  }

  function render(d) {
    last = d;
    dirty = false;
    const sec = d.settings.security;
    const creds = d.credentials;
    const ro = ctx.readOnly;
    const sw = toggleSwitch('Mocked security', sec.enabled, (v) => setEnabled(v, sw), { disabled: ro });
    // Credentials form.
    const modeSel = select(MODES.map(([v, l]) => [v, l]), sec.mode, { 'aria-label': 'Security mode' });
    const modeHint = h('span', { class: 'hint' });
    const headerIn = input({ value: sec.api_key.header, class: 'input mono', 'aria-label': 'API key header' });
    const keys = listEditor(sec.api_key.keys, 'demo-key', 'API key');
    const users = usersEditor(sec.basic.users);
    const tokens = listEditor(sec.bearer.tokens, 'demo-token', 'Bearer token');
    const issIn = input({ value: (sec.jwt && sec.jwt.issuer) || '', class: 'input mono', placeholder: 'any issuer', 'aria-label': 'Required issuer' });
    const audIn = input({ value: (sec.jwt && sec.jwt.audience) || '', class: 'input mono', placeholder: 'any audience', 'aria-label': 'Required audience' });
    const specTypes = new Set((d.spec.security_schemes || []).map((s) => s.type));
    const sections = {
      header: h('label', { class: 'field' }, h('span', null, 'API key header'), headerIn),
      keys: h('div', { class: 'field' }, h('span', null, 'API keys'), keys.el),
      users: h('div', { class: 'field' }, h('span', null, 'Basic auth users'), users.el),
      tokens: h('div', { class: 'field' }, h('span', null, 'Static bearer tokens'), tokens.el, h('span', { class: 'hint' }, 'Accepted as is; JWTs from the built-in IdP are accepted too.')),
      jwt: h('div', { class: 'grid grid-2' }, h('label', { class: 'field' }, h('span', null, 'JWT issuer (iss)'), issIn), h('label', { class: 'field' }, h('span', null, 'JWT audience (aud)'), audIn)),
    };
    function showSections() {
      const m = modeSel.value;
      modeHint.textContent = (MODES.find(([v]) => v === m) || [])[2] || '';
      const show = {
        header: m === 'api_key',
        keys: m === 'api_key' || (m === 'spec' && specTypes.has('api_key')),
        users: m === 'basic' || (m === 'spec' && specTypes.has('basic')),
        tokens: m === 'bearer' || (m === 'spec' && specTypes.has('bearer')),
        jwt: m === 'jwt' || m === 'bearer' || (m === 'spec' && (specTypes.has('bearer') || specTypes.has('oauth2') || specTypes.has('open_id_connect'))),
      };
      for (const [k, node] of Object.entries(sections)) node.hidden = !show[k];
    }
    modeSel.addEventListener('change', showSections);
    showSections();
    const saveBtn = h('button', { class: 'btn primary', type: 'button' }, 'Save to draft');
    const form = h('fieldset', { class: 'plain stack-sm', disabled: ro },
      h('label', { class: 'field' }, h('span', null, 'Mode'), modeSel, modeHint),
      ...Object.values(sections),
      ro ? null : h('div', { class: 'row' }, h('span', { class: 'grow' }), saveBtn));
    form.addEventListener('input', () => { dirty = true; });
    form.addEventListener('change', () => { dirty = true; });
    saveBtn.addEventListener('click', async () => {
      const next = {
        enabled: sec.enabled,
        mode: modeSel.value,
        api_key: { header: headerIn.value.trim() || 'x-api-key', keys: keys.get() },
        basic: { users: users.get() },
        bearer: { tokens: tokens.get() },
        jwt: {},
      };
      if (issIn.value.trim()) next.jwt.issuer = issIn.value.trim();
      if (audIn.value.trim()) next.jwt.audience = audIn.value.trim();
      saveBtn.disabled = true;
      const r = await api('PATCH', '/' + enc(ctx.name), { security: next });
      saveBtn.disabled = false;
      if (report(r, 'Saved to the draft. Deploy to publish it.', 'Saving')) { dirty = false; ctx.refresh(); }
    });
    // How to call it.
    const schemes = (creds.schemes || []).map((s) => {
      const lines = useLines(s, sec);
      return h('div', { class: 'scheme' },
        h('div', { class: 'row' }, icon('lock'), h('strong', { class: 'mono' }, s.name), badge(String(s.type).replace(/_/g, ' '), 'violet'), s.bearer_format ? badge(s.bearer_format, 'outline') : null),
        s.hint ? h('p', { class: 'small muted', style: { margin: '4px 0' } }, s.hint) : null,
        ...lines.map((line) => h('div', { class: 'row nowrap use-line' }, h('span', { class: 'tiny muted' }, 'Use'), h('code', { class: 'grow break' }, line), copyButton(line, { icon: true, title: 'Copy ' + line, toast: 'Copied' }))),
        ['oauth2', 'open_id_connect', 'bearer', 'jwt'].includes(s.type) ? h('div', { class: 'row nowrap use-line' }, h('span', { class: 'tiny muted' }, 'Or'), h('code', { class: 'grow break' }, 'Authorization: Bearer <JWT from Get a token>')) : null);
    });
    // Per-operation access.
    const opSettings = d.settings.operations || {};
    const rows = (d.operations || []).map((op) => {
      const own = opSettings[op.id] || {};
      const relevant = sec.mode !== 'spec' || op.spec_security;
      const pubSw = toggleSwitch('Public: ' + op.method + ' ' + op.path, !!own.public, async (v) => {
        const next = Object.assign({}, own, { public: v });
        if (!v) delete next.public;
        const r = await api('PUT', `/${enc(ctx.name)}/operations/${enc(op.id)}`, Object.keys(next).length ? next : null);
        if (report(r, v ? `${op.method} ${op.path} is public in the draft` : `${op.method} ${op.path} is secured in the draft`, 'Saving')) ctx.refresh();
        else pubSw.set(!v);
      }, { disabled: ro || !relevant });
      return h('tr', null,
        h('td', null, methodBadge(op.method)), h('td', { class: 'mono break' }, op.path),
        h('td', null, op.spec_security ? badge(h('span', null, icon('lock'), ' declared'), 'outline') : h('span', { class: 'tiny muted' }, 'none')),
        h('td', null, op.secured ? badge(h('span', null, icon('lock'), ' secured'), 'violet') : badge('open', '')),
        h('td', null, h('div', { class: 'row nowrap' }, pubSw.el, h('span', { class: 'tiny muted' }, relevant ? (own.public ? 'public' : 'checked') : 'nothing to check'))));
    });
    replace(el,
      card('Mocked security', { actions: h('div', { class: 'row nowrap' }, sw.el, h('strong', null, sec.enabled ? 'On' : 'Off')) },
        h('div', { class: 'stack-sm' },
          h('p', { class: 'small', style: { margin: 0 } }, sec.enabled
            ? 'Calls without valid credentials get 401 (or 403 for missing OAuth2 scopes), like the real API behind a gateway would answer.'
            : 'Off: every call is answered without credentials. Switch it on to demo authentication; the switch deploys at once.'),
          d.spec_declares_security ? null : h('p', { class: 'small muted', style: { margin: 0 } }, 'The spec declares no security schemes: pick API key, basic, bearer or JWT below to protect every operation.'))),
      h('div', { class: 'grid grid-2 sec-grid' },
        card('Mode and credentials', { hint: 'saved to the draft' }, form),
        card('How to call it', { actions: h('button', { class: 'btn sm', type: 'button', onclick: getToken }, icon('token'), 'Get a token') },
          h('div', { class: 'stack-sm' },
            schemes.length ? schemes : h('p', { class: 'small muted' }, 'No scheme applies (the spec declares none and the mode is "spec").'),
            tokenEl,
            sec.enabled ? null : notice('', 'Security is off, so these credentials are not checked yet.')))),
      card('Per-operation access', { hint: 'public operations skip the check; changes go to the draft', flush: true },
        h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Method'), h('th', null, 'Path'), h('th', null, 'Spec'), h('th', null, 'Now'), h('th', null, 'Public'))),
          h('tbody', null, ...rows)))));
  }

  return {
    el,
    update(d) { if (!dirty || !last) render(d); },
  };
}

// ── Traffic ────────────────────────────────────────────────────────

const OUTCOMES = {
  mocked: ['mocked', 'ok'], not_found: ['no operation', 'warn'], method_not_allowed: ['method not allowed', 'warn'],
  unauthorized: ['unauthorized', 'err'], forbidden: ['forbidden', 'err'], invalid_request: ['invalid request', 'err'],
  disabled: ['switched off', ''], not_deployed: ['not deployed', ''],
};

export function outcomeBadge(o) {
  const [label, kind] = OUTCOMES[o] || [o, ''];
  return h('span', { class: 'badge ' + kind }, label);
}

/** ctx: {name, readOnly, detail(), refresh()}. Returns {el, show(), destroy(), clearLocal()}. */
export function trafficPanel(ctx) {
  let started = false;
  let feed = null;
  let retryTimer = null;
  let lastSeq = 0;
  let total = 0;
  let selected = null;
  let entries = [];
  let gone = false;
  const MAX = 300;
  const dot = h('span', { class: 'dot warn' });
  const stateText = h('span', { class: 'small muted' }, 'connecting');
  const countEl = h('span', { class: 'small muted' });
  const listEl = h('div', { class: 'list' });
  const detailEl = h('div');
  const clearBtn = ctx.readOnly ? null : h('button', { class: 'btn sm', type: 'button', onclick: clearAll }, icon('trash'), 'Clear');
  const testBtn = h('button', { class: 'btn sm', type: 'button', onclick: sendTest }, icon('send'), 'Send a test request');
  const el = h('div', { class: 'stack' },
    h('div', { class: 'card' },
      h('div', { class: 'card-head' }, h('h2', null, 'Traffic'), h('span', { class: 'row nowrap', style: { gap: '6px' } }, dot, stateText), countEl, testBtn, clearBtn),
      h('div', { class: 'split', style: { minHeight: '380px' } },
        h('div', { class: 'pane' }, h('div', { class: 'pane-scroll' }, listEl)),
        h('div', { class: 'pane' }, h('div', { class: 'pane-scroll' }, detailEl)))));

  function setState(kind, text) {
    dot.className = 'dot ' + kind;
    stateText.textContent = text;
  }

  function add(e, live) {
    if (entries.some((x) => x.seq === e.seq)) return;
    entries.unshift(e);
    entries.sort((a, b) => b.seq - a.seq);
    if (entries.length > MAX) entries.length = MAX;
    lastSeq = Math.max(lastSeq, e.seq);
    if (live) total++;
    if (!selected) selected = e.seq;
  }

  async function backfill() {
    const r = await api('GET', `/${enc(ctx.name)}/requests?limit=100`);
    if (r.status === 404) { gone = true; setState('err', 'mock not found'); return; }
    if (!r.ok || !r.json) { setState('err', errorText(r, 'Loading the traffic')); return; }
    total = r.json.total;
    for (const e of (r.json.requests || []).slice().reverse()) add(e, false);
    render();
  }

  async function follow() {
    if (gone) return;
    const ctrl = new AbortController();
    feed = ctrl;
    const headers = [['Accept', 'text/event-stream']];
    if (lastSeq) headers.push(['Last-Event-ID', String(lastSeq)]);
    const r = await send({ url: SERVER + '/_rustybin/mocks/' + enc(ctx.name) + '/requests/stream', headers, stream: true, session: false, timeout: 15000, signal: ctrl.signal });
    if (feed !== ctrl) return;
    if (r.error || !r.ok) {
      if (r.status === 404) { gone = true; setState('err', 'mock not found'); return; }
      if (r.status === 401 || r.status === 403) { setState('err', 'not allowed to read the feed'); return; }
      retry(r.error || 'feed answered ' + r.status);
      return;
    }
    setState('live', 'live');
    const parse = sseParser((ev) => {
      if (ev.event !== 'request') return;
      try { add(JSON.parse(ev.data), true); render(); } catch { /* ignore malformed */ }
    });
    try { await readText(r.response, parse, ctrl.signal); } catch { /* dropped */ }
    if (feed === ctrl && !ctrl.signal.aborted) retry('feed ended');
  }

  function retry(why) {
    setState('warn', 'reconnecting (' + why + ')');
    clearTimeout(retryTimer);
    retryTimer = setTimeout(() => { if (started) follow(); }, 2000);
  }

  function render() {
    countEl.textContent = entries.length ? `${entries.length} shown, ${total} received` : '';
    clear(listEl);
    if (!entries.length) {
      listEl.appendChild(empty('No calls yet', 'Send a request from Try it, curl or your gateway: it shows up here at once with the operation it matched, the validation result and the response sent.', 'inbox'));
      replace(detailEl);
      return;
    }
    for (const e of entries) {
      const valid = e.validation && e.validation.length ? h('span', { class: 'check-icon warn', title: e.validation.length + ' validation issue(s)' }, icon('alert')) : e.operation ? h('span', { class: 'check-icon ok', title: 'request matches the spec (or validation is off)' }, icon('check')) : h('span');
      listEl.appendChild(h('button', { class: 'list-item mt-row', type: 'button', 'aria-selected': String(e.seq === selected), onclick: () => { selected = e.seq; render(); } },
        h('span', { class: 'mono tiny muted' }, '#' + e.seq), methodBadge(e.method),
        h('span', { style: { minWidth: 0 } }, h('div', { class: 'mono ellipsis' }, e.path + (e.query ? '?' + e.query : '')),
          h('div', { class: 'tiny muted ellipsis' }, [e.operation || 'no operation', e.auth].filter(Boolean).join(' | '))),
        h('span', { class: 'st st-' + Math.floor(e.status / 100) }, String(e.status)),
        h('span', { class: 'mt-outcome' }, outcomeBadge(e.outcome)), valid,
        h('span', { class: 'mono tiny muted mt-time' }, fmtTime(e.timestamp).slice(0, 8))));
    }
    const cur = entries.find((x) => x.seq === selected) || entries[0];
    selected = cur.seq;
    renderDetail(cur);
  }

  function renderDetail(e) {
    const reqHeaders = e.headers.map((x) => [x.name, x.value]);
    const curl = toCurl({ method: e.method, url: SERVER + e.full_path + (e.query ? '?' + e.query : ''), headers: reqHeaders.filter(([k]) => !/^(host|content-length|connection|accept-encoding)$/i.test(k)), body: e.body || undefined });
    const issues = e.validation || [];
    replace(detailEl, h('div', null,
      h('div', { class: 'row', style: { padding: '10px 14px', borderBottom: '1px solid var(--border)' } },
        methodBadge(e.method), h('span', { class: 'mono break grow' }, e.full_path + (e.query ? '?' + e.query : '')),
        statusBadge(e.status), outcomeBadge(e.outcome)),
      tabs([
        { id: 'request', label: 'Request', render: () => h('div', { class: 'stack-sm', style: { padding: '12px' } },
          headersTable(e.headers), e.body_base64 ? codeBlock(e.body_base64) : bodyView(e.body, e.content_type, { cls: 'short' }),
          e.body_truncated ? h('p', { class: 'tiny muted' }, `Body cut to the first part (${fmtBytes(e.body_size)} received).`) : null,
          h('details', { class: 'disclosure' }, h('summary', null, 'As curl'), copyableCode(curl))) },
        { id: 'response', label: 'Response', render: () => h('div', { class: 'stack-sm', style: { padding: '12px' } },
          h('div', { class: 'row' }, statusBadge(e.status), e.source ? badge('source ' + e.source, 'info') : null, h('span', { class: 'tiny muted' }, fmtMs(e.duration_ms))),
          headersTable(e.response_headers, { noHighlight: true }),
          bodyView(e.response_body, (e.response_headers.find((x) => x.name === 'content-type') || {}).value, { cls: 'short' }),
          e.response_truncated ? h('p', { class: 'tiny muted' }, 'Only the start of the body is kept.') : null) },
        { id: 'validation', label: `Validation (${issues.length})`, render: () => h('div', { style: { padding: '12px' } },
          issues.length ? h('div', { class: 'stack-sm' }, notice('warn', 'The request does not match the spec. ', ctx.detail() && ctx.detail().validate_requests === 'enforce' ? 'Validation is enforced, so the mock answered 400.' : 'Validation warns, so the mock answered anyway (header X-Rustybin-Mock-Validation: failed).'), issuesList(issues))
            : h('p', { class: 'small muted' }, e.operation ? 'No problems found (or validation is off).' : 'Not validated: no operation matched.')) },
        { id: 'info', label: 'Details', render: () => h('div', { style: { padding: '12px' } }, kvgrid([
          ['Outcome', e.outcome], ['Operation', e.operation || null], ['Response source', e.source || null], ['Auth', e.auth || 'not required'],
          ['Revision', String(e.revision)], ['Duration', fmtMs(e.duration_ms)], ['Client IP', e.client_ip || null], ['Received', new Date(e.timestamp).toLocaleString() + ' (' + relTime(e.timestamp) + ')'], ['Request id', e.id]])) },
      ], issues.length ? 'validation' : 'request').el));
  }

  async function clearAll() {
    if (!confirm('Clear every captured call of this mock?')) return;
    const r = await api('DELETE', `/${enc(ctx.name)}/requests`);
    if (report(r, 'Traffic cleared', 'Clearing')) clearLocal();
  }

  function clearLocal() {
    entries = [];
    selected = null;
    total = 0;
    render();
  }

  async function sendTest() {
    const d = ctx.detail();
    const op = (d.operations || []).find((o) => o.method === 'GET') || (d.operations || [])[0];
    if (!op) { toast('This mock has no operations', 'err'); return; }
    const r0 = await api('GET', `/${enc(ctx.name)}/operations/${enc(op.id)}`);
    if (!r0.ok || !r0.json) { toast(errorText(r0), 'err'); return; }
    const req = sampleToRequest(ctx.name, r0.json.sample_request);
    const r = await send({ method: req.method, url: req.url, headers: req.headers, body: req.body, timeout: 30000 });
    if (r.error) toast(r.error, 'err');
    else toast(`${req.method} ${req.path} answered ${r.status}`);
  }

  return {
    el,
    show() {
      if (started) return;
      started = true;
      render();
      backfill().then(follow);
    },
    clearLocal,
    destroy() {
      started = false;
      clearTimeout(retryTimer);
      if (feed) { const f = feed; feed = null; f.abort(); }
    },
  };
}
