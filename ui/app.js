// Console shell: navigation, hash router, theme, settings dialog and the
// global live traffic feed. Views live in views/*.js and export
// `{ id, title, icon, mount(el) -> cleanup }`.
import { h, icon, replace, clear, notice, toast, field, input, select, fmtDuration } from './lib/dom.js';
import * as settings from './lib/settings.js';
import * as traffic from './lib/traffic.js';
import * as auth from './lib/auth.js';
import { SERVER, getJson, send, trimBase } from './lib/http.js';

const VIEWS = [
  { section: 'Instance' },
  { id: 'overview', title: 'Overview', icon: 'overview', load: () => import('./views/overview.js') },
  { id: 'traffic', title: 'Live traffic', icon: 'traffic', load: () => import('./views/traffic.js') },
  { id: 'bins', title: 'Request bins', icon: 'bin', load: () => import('./views/bins.js') },
  { id: 'explorer', title: 'API explorer', icon: 'explorer', load: () => import('./views/explorer.js') },
  { section: 'AI and agents' },
  { id: 'ai', title: 'AI playground', icon: 'ai', load: () => import('./views/ai.js') },
  { id: 'mcp', title: 'MCP inspector', icon: 'mcp', load: () => import('./views/mcp.js') },
  { id: 'a2a', title: 'A2A client', icon: 'a2a', load: () => import('./views/a2a.js') },
  { section: 'Resilience and security' },
  { id: 'chaos', title: 'Chaos and health', icon: 'chaos', load: () => import('./views/chaos.js') },
  { id: 'tokens', title: 'Token lab', icon: 'token', load: () => import('./views/tokens.js') },
];

const app = document.getElementById('app');
const viewEl = document.getElementById('view');
const nav = document.getElementById('sidebar');
const meta = document.getElementById('topbar-meta');
const actions = document.getElementById('topbar-actions');
const dialog = document.getElementById('settings-dialog');
let cleanup = null;
let currentId = null;
let navToken = 0;
const navLinks = new Map();

// ── Theme ──────────────────────────────────────────────────────────

function effectiveTheme() {
  const t = settings.get('theme');
  if (t === 'light' || t === 'dark') return t;
  return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function applyTheme() {
  const t = settings.get('theme');
  if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
  else document.documentElement.removeAttribute('data-theme');
  const btn = document.getElementById('theme-btn');
  if (btn) {
    const dark = effectiveTheme() === 'dark';
    replace(btn, icon(dark ? 'sun' : 'moon'));
    btn.setAttribute('aria-label', dark ? 'Switch to light theme' : 'Switch to dark theme');
    btn.title = btn.getAttribute('aria-label');
  }
}

// ── Navigation ─────────────────────────────────────────────────────

function buildNav() {
  clear(nav);
  for (const v of VIEWS) {
    if (v.section) {
      nav.appendChild(h('div', { class: 'nav-section' }, v.section));
      continue;
    }
    const count = h('span', { class: 'nav-count', hidden: true });
    const a = h('a', { class: 'nav-link', href: '#/' + v.id }, icon(v.icon), h('span', null, v.title), count);
    a.addEventListener('click', () => app.classList.remove('nav-open'));
    navLinks.set(v.id, { a, count });
    nav.appendChild(a);
  }
  nav.appendChild(h('div', { class: 'sidebar-foot' },
    h('a', { href: SERVER + '/' }, 'Endpoint list'),
    h('a', { href: SERVER + '/docs' }, 'OpenAPI docs'),
    h('span', { id: 'foot-version' }, '')));
}

function updateTrafficCount() {
  const link = navLinks.get('traffic');
  if (!link) return;
  const n = traffic.unseenCount();
  link.count.hidden = n === 0;
  link.count.textContent = n > 99 ? '99+' : String(n);
  link.count.setAttribute('aria-label', n + ' new requests');
}

function parseRoute() {
  const m = /^#\/([\w-]+)(?:\/(.*))?$/.exec(location.hash || '');
  const id = m && VIEWS.some((v) => v.id === m[1]) ? m[1] : 'overview';
  return { id, rest: m && m[2] ? decodeURIComponent(m[2]) : '' };
}

async function route() {
  const { id, rest } = parseRoute();
  if (id === currentId && cleanup && cleanup.onRoute) {
    cleanup.onRoute(rest);
    return;
  }
  const token = ++navToken;
  if (cleanup) {
    try { (typeof cleanup === 'function' ? cleanup : cleanup.destroy)(); } catch (e) { console.error(e); }
    cleanup = null;
  }
  currentId = id;
  traffic.setViewing(id === 'traffic');
  updateTrafficCount();
  for (const [vid, { a }] of navLinks) {
    if (vid === id) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  const def = VIEWS.find((v) => v.id === id);
  if (!auth.needsSignIn()) document.title = def.title + ' | ' + (auth.consoleTitle ? auth.consoleTitle + ' | ' : '') + 'Rustybin console';
  replace(viewEl, h('div', { class: 'row muted' }, h('span', { class: 'spinner' }), 'Loading ' + def.title));
  try {
    const mod = await def.load();
    if (token !== navToken) return;
    clear(viewEl);
    cleanup = mod.default.mount(viewEl, { rest }) || null;
  } catch (e) {
    if (token !== navToken) return;
    console.error(e);
    replace(viewEl, notice('err', 'This view failed to load: ' + (e && e.message ? e.message : e)));
  }
  document.getElementById('main').scrollTop = 0;
}

// ── Top bar ────────────────────────────────────────────────────────

function chip(label, value, title) {
  return h('button', { class: 'btn sm ghost', type: 'button', title, onclick: openSettings },
    h('span', { class: 'muted' }, label), h('span', { class: 'mono ellipsis', style: { maxWidth: '180px' } }, value));
}

function renderMeta() {
  clear(meta);
  if (auth.consoleTitle) meta.appendChild(h('span', { class: 'badge violet console-title', title: 'RUSTYBIN_CONSOLE_TITLE' }, auth.consoleTitle));
  const st = settings.instance.status;
  if (st) {
    const healthy = st.healthy;
    meta.appendChild(h('span', { class: 'badge ' + (healthy ? 'ok' : 'err'), title: healthy ? 'GET /health returns 200' : 'GET /health returns 503' },
      h('span', { class: 'dot ' + (healthy ? 'ok' : 'err') }), healthy ? 'healthy' : 'unhealthy'));
    if (st.public_mode) meta.appendChild(h('span', { class: 'badge warn', title: 'RUSTYBIN_PUBLIC_MODE: tighter limits, session scoped data' }, 'public mode'));
  }
  const ts = traffic.getStatus();
  const live = ts.status === 'live';
  meta.appendChild(h('span', { class: 'badge outline hide-sm', title: ts.error || 'Live inspector feed' },
    h('span', { class: 'dot ' + (live ? 'live' : ts.status === 'error' || ts.status === 'needs-session' ? 'err' : 'warn') }),
    live ? 'live feed' : ts.status === 'needs-session' ? 'set a session' : ts.status === 'signed-out' ? 'signed out' : ts.status));
}

function backlinkLabel() {
  if (auth.consoleTitle) return auth.consoleTitle;
  try { return new URL(auth.backlink).host; } catch { return 'portal'; }
}

function renderActions() {
  clear(actions);
  if (auth.backlink) {
    actions.appendChild(h('a', { class: 'btn sm ghost backlink', href: auth.backlink, rel: 'noopener', title: 'Back to ' + auth.backlink, id: 'backlink' },
      icon('external'), h('span', { class: 'ellipsis', style: { maxWidth: '200px' } }, 'Back to ' + backlinkLabel())));
  }
  const gw = settings.get('gatewayUrl');
  const session = settings.get('session');
  const wrap = h('span', { class: 'row nowrap topbar-field hide-sm' });
  if (gw) wrap.appendChild(chip('Gateway', trimBase(gw).replace(/^https?:\/\//, ''), 'Gateway base URL used by "via gateway" requests'));
  wrap.appendChild(chip('Session', session || 'none', 'X-Rustybin-Session sent with console requests'));
  actions.appendChild(wrap);
  actions.appendChild(h('button', { class: 'btn ghost icon', type: 'button', id: 'settings-btn', 'aria-label': 'Console settings', title: 'Console settings', onclick: openSettings }, icon('settings')));
  const themeBtn = h('button', { class: 'btn ghost icon', type: 'button', id: 'theme-btn' });
  themeBtn.addEventListener('click', () => settings.set('theme', effectiveTheme() === 'dark' ? 'light' : 'dark'));
  actions.appendChild(themeBtn);
  applyTheme();
}

function openSettings() {
  const session = input({ value: settings.get('session'), placeholder: 'e.g. demo-acme', 'aria-describedby': 'session-hint' });
  const gateway = input({ value: settings.get('gatewayUrl'), placeholder: 'https://gateway.example.com/rustybin', type: 'url' });
  const admin = input({ value: settings.get('adminToken'), placeholder: 'only when RUSTYBIN_ADMIN_TOKEN is set', type: 'password' });
  const theme = select([['system', 'Follow the system'], ['light', 'Light'], ['dark', 'Dark']], settings.get('theme'));
  const err = h('div');
  const exp = auth.expiresAt();
  const signedIn = auth.required() && auth.getToken()
    ? h('div', { class: 'notice' }, icon('shield'), h('div', { class: 'grow' },
      'Signed in to the control plane (RUSTYBIN_CONTROL_AUTH=' + auth.mode + ')',
      auth.scopes() ? ', scope ' + auth.scopes().join(' ') : '',
      exp ? ', expires ' + new Date(exp).toLocaleTimeString() : '', '.'),
    h('button', { class: 'btn sm', type: 'button', id: 'sign-out', onclick: () => { dialog.close(); auth.signOut(); } }, 'Sign out'))
    : null;
  const form = h('form', { method: 'dialog', class: 'stack', style: { padding: '18px', width: 'min(560px, 92vw)' } },
    h('h2', { id: 'settings-title' }, 'Console settings'),
    signedIn,
    field('Session (X-Rustybin-Session)', session, 'Tags the requests this console sends, scopes per-client state (flaky counters, bins, tasks). Required to see traffic in public mode. Letters, digits and . _ : - only.'),
    field('Gateway base URL', gateway, 'Views offer "via gateway" to send their requests through this URL instead of directly to Rustybin. Cross-origin calls need CORS on the gateway.'),
    field('Admin token', admin, settings.instance.adminTokenConfigured ? 'This instance requires it for health toggles and clearing all captured requests.' : 'Not required by this instance.'),
    field('Theme', theme),
    err,
    h('div', { class: 'row' }, h('span', { class: 'grow' }),
      h('button', { class: 'btn', type: 'button', onclick: () => dialog.close() }, 'Cancel'),
      h('button', { class: 'btn primary', type: 'submit' }, 'Save')));
  form.addEventListener('submit', (e) => {
    const sv = session.value.trim();
    if (sv && !/^[A-Za-z0-9._:-]{1,128}$/.test(sv)) {
      e.preventDefault();
      replace(err, notice('err', 'The session may only contain letters, digits and . _ : - (at most 128 characters).'));
      return;
    }
    const gv = trimBase(gateway.value);
    if (gv && !/^https?:\/\/[^\s]+$/.test(gv)) {
      e.preventDefault();
      replace(err, notice('err', 'The gateway URL must start with http:// or https://'));
      return;
    }
    settings.set('session', sv);
    settings.set('gatewayUrl', gv);
    settings.set('adminToken', admin.value.trim());
    settings.set('theme', theme.value);
    toast('Settings saved');
  });
  replace(dialog, form);
  dialog.showModal();
  session.focus();
}

// ── OAuth callback (authorization code flow of the token lab) ──────

function handleCallback() {
  if (!/\/ui\/callback\/?$/.test(location.pathname)) return false;
  const params = location.search;
  if (window.opener && !window.opener.closed) {
    try {
      window.opener.postMessage({ type: 'rustybin-oauth-callback', search: params }, location.origin);
      document.body.textContent = 'Signed in. You can close this window.';
      setTimeout(() => window.close(), 300);
      return true;
    } catch { /* fall through to same-window handling */ }
  }
  try { sessionStorage.setItem('rustybin.oauth.callback', params); } catch { /* ignore */ }
  location.replace(location.pathname.replace(/callback\/?$/, '') + '#/tokens');
  return true;
}

// ── Sign-in screen (RUSTYBIN_CONTROL_AUTH = token | jwt) ───────────

let signinEl = null;

const REASONS = {
  missing: null,
  'signed-out': 'You signed out.',
  expired: 'Your session expired.',
  invalid_audience: 'The token was issued for another instance (audience mismatch).',
  invalid_signature: 'The token signature is not valid for this instance.',
  invalid_algorithm: 'The token uses an algorithm this instance does not accept.',
  not_yet_valid: 'The token is not valid yet (check the clock).',
  missing_claim: 'The token lacks a required claim (exp, aud).',
  malformed: 'The token is malformed.',
  invalid_token: 'The token was refused.',
  missing_token: 'The token did not reach the server.',
};

function renderSignIn() {
  if (!auth.needsSignIn()) {
    if (signinEl) { signinEl.remove(); signinEl = null; }
    app.removeAttribute('aria-hidden');
    return;
  }
  const reason = auth.getReason();
  const expired = reason === 'expired';
  const tokenInput = input({ type: 'password', placeholder: auth.mode === 'jwt' ? 'Access token or admin token' : 'Admin token', 'aria-label': 'Token', autocomplete: 'off', id: 'signin-token' });
  const err = h('div');
  const why = REASONS[reason] === undefined ? 'The token was refused (' + reason + ').' : REASONS[reason];
  const form = h('form', { class: 'stack' },
    field(auth.mode === 'jwt' ? 'Paste a token' : 'Paste the admin token', tokenInput,
      auth.mode === 'jwt' ? 'A control-plane access token (JWT) or RUSTYBIN_ADMIN_TOKEN. It is kept in this tab only.' : 'RUSTYBIN_ADMIN_TOKEN. It is kept in this tab only.'),
    err,
    h('div', { class: 'row' }, h('span', { class: 'grow' }), h('button', { class: 'btn primary', type: 'submit' }, 'Sign in')));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const v = tokenInput.value.trim();
    if (!v) {
      replace(err, notice('err', 'Enter a token.'));
      return;
    }
    auth.setToken(v);
  });
  const body = h('div', { class: 'stack' },
    h('div', { class: 'row nowrap' }, icon('shield'), h('h1', { id: 'signin-title' }, expired ? 'Session expired' : 'Sign in required')),
    why ? notice(expired || reason === 'signed-out' ? 'warn' : 'err', why) : null,
    h('p', null, 'This Rustybin instance protects its control plane and console data (RUSTYBIN_CONTROL_AUTH=' + auth.mode + '). ',
      auth.backlink ? 'Open the console again from your portal to get a fresh access token, or paste a token below.' : 'Paste a token below, or open the console with #token=<token> appended to its URL.'),
    auth.backlink ? h('a', { class: 'btn primary', href: auth.backlink, rel: 'noopener', id: 'signin-backlink' }, icon('external'), 'Open ' + backlinkLabel()) : null,
    form,
    h('p', { class: 'muted small' }, 'The data plane stays open: ', h('a', { href: SERVER + '/' }, 'endpoint list'), ', ', h('a', { href: SERVER + '/docs' }, 'OpenAPI docs'), '.'));
  const el = h('div', { class: 'signin', id: 'signin', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'signin-title' },
    h('section', { class: 'card signin-card' }, h('div', { class: 'card-body' }, body)));
  if (signinEl) signinEl.replaceWith(el);
  else document.body.appendChild(el);
  signinEl = el;
  app.setAttribute('aria-hidden', 'true');
  document.title = (expired ? 'Session expired' : 'Sign in') + ' | ' + (auth.consoleTitle ? auth.consoleTitle + ' | ' : '') + 'Rustybin console';
  tokenInput.focus();
}

async function verifyAndResume() {
  // Check the new token once; a 401 brings the sign-in screen back with the reason.
  const r = await send({ url: SERVER + '/_rustybin/status', session: false, timeout: 15000 });
  if (r.ok) {
    renderSignIn();
    currentId = null;
    route();
    loadInstance().then(() => traffic.connect());
  }
}

// ── Start ──────────────────────────────────────────────────────────

async function loadInstance() {
  if (auth.needsSignIn()) {
    renderMeta();
    return;
  }
  try {
    const st = await getJson('/_rustybin/status', { session: false });
    settings.instance.status = st;
    settings.instance.publicMode = !!st.public_mode;
    settings.instance.adminTokenConfigured = !!st.admin_token_configured;
    const foot = document.getElementById('foot-version');
    if (foot) foot.textContent = 'v' + st.version + ' | up ' + fmtDuration(st.uptime_seconds);
    if (st.public_mode && !settings.get('session')) {
      const b = new Uint8Array(4);
      crypto.getRandomValues(b);
      settings.set('session', 'console-' + Array.from(b, (x) => x.toString(16).padStart(2, '0')).join(''));
    }
  } catch (e) {
    settings.instance.status = null;
    console.warn('status unavailable', e);
  }
  renderMeta();
}

function start() {
  if (handleCallback()) return;
  auth.consumeFragment();
  const menu = document.getElementById('menu-btn');
  menu.appendChild(icon('menu'));
  menu.addEventListener('click', () => {
    const open = app.classList.toggle('nav-open');
    menu.setAttribute('aria-expanded', String(open));
  });
  document.getElementById('backdrop').addEventListener('click', () => app.classList.remove('nav-open'));
  buildNav();
  renderActions();
  applyTheme();
  if (window.matchMedia) window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', applyTheme);
  settings.onChange((key) => {
    if (key === 'theme') applyTheme();
    if (key === 'session' || key === 'gatewayUrl') renderActions();
  });
  traffic.subscribe((kind) => {
    if (kind === 'status') renderMeta();
    if (kind === 'add' || kind === 'seen' || kind === 'clear') updateTrafficCount();
  });
  window.addEventListener('hashchange', () => {
    // A token pasted into the address bar as #token=... is picked up too.
    if (auth.consumeFragment()) return;
    route();
  });
  auth.onChange((kind) => {
    if (kind === 'token') {
      renderActions();
      verifyAndResume();
    } else {
      renderSignIn();
      renderMeta();
    }
  });
  renderSignIn();
  document.addEventListener('keydown', (e) => {
    if (e.altKey && !e.ctrlKey && !e.metaKey && /^[1-9]$/.test(e.key)) {
      const views = VIEWS.filter((v) => v.id);
      const v = views[Number(e.key) - 1];
      if (v) { location.hash = '#/' + v.id; e.preventDefault(); }
    }
  });
  route();
  loadInstance().then(() => traffic.connect());
  // Refresh the health badge and footer now and then (cheap control-plane call).
  setInterval(loadInstance, 15000);
}

window.rustybinConsole = { settings, traffic };
start();
