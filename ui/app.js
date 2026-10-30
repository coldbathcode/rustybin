// Console shell: navigation, hash router, theme and the global live
// traffic feed. Views live in views/*.js and export
// `{ id, title, icon, mount(el) -> cleanup }`. The gear in the top bar opens
// the Management view (instance settings and this browser's).
import { h, icon, replace, clear, notice, toast, input, field, checkbox, fmtDuration } from './lib/dom.js';
import * as settings from './lib/settings.js';
import * as traffic from './lib/traffic.js';
import * as auth from './lib/auth.js';
import * as visit from './lib/visit.js';
import * as assist from './lib/assist.js';
import { SERVER, getJson, send, trimBase } from './lib/http.js';

const VIEWS = [
  { section: 'Instance' },
  // `ai: true` marks views with AI actions: a sparkle in the navigation when assist is on.
  { id: 'overview', title: 'Overview', icon: 'overview', ai: true, load: () => import('./views/overview.js') },
  { id: 'traffic', title: 'Live traffic', icon: 'traffic', ai: true, load: () => import('./views/traffic.js') },
  { id: 'mocks', title: 'Mock APIs', icon: 'mock', ai: true, load: () => import('./views/mocks.js') },
  { id: 'bins', title: 'Request bins', icon: 'bin', load: () => import('./views/bins.js') },
  { id: 'explorer', title: 'API explorer', icon: 'explorer', load: () => import('./views/explorer.js') },
  { id: 'manage', title: 'Management', icon: 'settings', load: () => import('./views/manage.js') },
  { section: 'AI and agents' },
  { id: 'ai', title: 'AI playground', icon: 'ai', load: () => import('./views/ai.js') },
  { id: 'mcp', title: 'MCP inspector', icon: 'mcp', ai: true, load: () => import('./views/mcp.js') },
  { id: 'registry', title: 'MCP registry', icon: 'data', ai: true, load: () => import('./views/registry.js') },
  { id: 'a2a', title: 'A2A client', icon: 'a2a', load: () => import('./views/a2a.js') },
  { section: 'Resilience and security' },
  { id: 'chaos', title: 'Chaos and health', icon: 'chaos', load: () => import('./views/chaos.js') },
  // Shown once /_rustybin/status says harness mode is on (RUSTYBIN_HARNESS=on).
  { id: 'harness', title: 'Harness', icon: 'send', hidden: true, feature: 'harness', ai: true, load: () => import('./views/harness.js') },
  { id: 'tokens', title: 'Token lab', icon: 'token', load: () => import('./views/tokens.js') },
  // Reached from an agent's sign-in (/_rustybin/oauth/authorize), never from the navigation.
  { id: 'authorize', title: 'Sign in an AI agent', icon: 'shield', hidden: true, load: () => import('./views/authorize.js') },
];

const app = document.getElementById('app');
const viewEl = document.getElementById('view');
const bannerEl = document.getElementById('banner');
const nav = document.getElementById('sidebar');
const meta = document.getElementById('topbar-meta');
const actions = document.getElementById('topbar-actions');
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
    if (v.hidden) continue;
    const count = h('span', { class: 'nav-count', hidden: true });
    const spark = v.ai ? icon('ai', 'nav-spark') : null;
    const a = h('a', { class: 'nav-link', href: '#/' + v.id }, icon(v.icon), h('span', null, v.title), spark, count);
    a.addEventListener('click', () => app.classList.remove('nav-open'));
    navLinks.set(v.id, { a, count });
    nav.appendChild(a);
  }
  nav.appendChild(h('div', { class: 'sidebar-foot' },
    h('a', { href: SERVER + '/' }, 'Endpoint list'),
    h('a', { href: SERVER + '/docs' }, 'OpenAPI docs'),
    h('span', { id: 'foot-version' }, '')));
}

// Views behind an instance feature (harness mode) appear in the navigation
// once the status reports the feature on.
function showFeatureViews(st) {
  let changed = false;
  for (const v of VIEWS) {
    if (!v.feature) continue;
    const on = !!(st && st[v.feature] && st[v.feature].enabled);
    if (v.hidden === !on) continue;
    v.hidden = !on;
    changed = true;
  }
  if (changed) {
    navLinks.clear();
    buildNav();
    for (const [vid, { a }] of navLinks) {
      if (vid === currentId) a.setAttribute('aria-current', 'page');
    }
    updateTrafficCount();
  }
}

// Sparkles next to the views with AI actions, once assist is on.
function showAiMarks() {
  nav.classList.toggle('assist-on', assist.enabled());
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
  assist.setView(id, null);
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
    // A view may describe what it shows for the assistant (cleanup.assistContext).
    if (cleanup && typeof cleanup.assistContext === 'function') assist.setView(id, cleanup.assistContext);
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
    // Shared mode: signed in for one API key, which sees its own data only.
    if (st.key && st.key.id) meta.appendChild(h('span', { class: 'badge outline', title: 'Signed in for this API key: its requests, usage and mocks only' }, 'key ' + st.key.id));
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

let assistBarEl = null;
function renderActions() {
  // Keep the mounted bar (it holds its own state) across re-renders.
  if (!assistBarEl) assistBarEl = document.getElementById('assist-wrap');
  clear(actions);
  if (auth.backlink) {
    actions.appendChild(h('a', { class: 'btn sm ghost backlink', href: auth.backlink, rel: 'noopener', title: 'Back to ' + auth.backlink, id: 'backlink' },
      icon('external'), h('span', { class: 'ellipsis', style: { maxWidth: '200px' } }, 'Back to ' + backlinkLabel())));
  }
  // The assist bar sits before the chips; mounted once, re-attached on re-render.
  const barHost = h('div', { class: 'assist-host hide-sm' });
  actions.appendChild(barHost);
  if (assistBarEl) barHost.appendChild(assistBarEl);
  else assistBarEl = assist.mountBar(barHost);
  const gw = settings.get('gatewayUrl');
  const session = settings.get('session');
  const wrap = h('span', { class: 'row nowrap topbar-field hide-sm' });
  if (gw) wrap.appendChild(chip('Gateway', trimBase(gw).replace(/^https?:\/\//, ''), 'Gateway base URL used by "via gateway" requests'));
  wrap.appendChild(chip('Session', session || 'none', 'X-Rustybin-Session sent with console requests'));
  actions.appendChild(wrap);
  if (auth.required() && auth.getToken()) {
    const who = auth.user() || (auth.signedInWithPassword() ? 'signed in' : 'token');
    actions.appendChild(h('button', { class: 'btn sm ghost signed-in hide-sm', type: 'button', id: 'signed-in', title: signedInTitle(), onclick: () => { location.hash = '#/manage/signin'; } },
      icon('user'), h('span', { class: 'ellipsis' }, who), h('span', { class: 'muted', id: 'signed-in-expiry' }, expiryText())));
  }
  actions.appendChild(h('button', { class: 'btn ghost icon', type: 'button', id: 'settings-btn', 'aria-label': 'Console settings', title: 'Console settings', onclick: openSettings }, icon('settings')));
  const themeBtn = h('button', { class: 'btn ghost icon', type: 'button', id: 'theme-btn' });
  themeBtn.addEventListener('click', () => settings.set('theme', effectiveTheme() === 'dark' ? 'light' : 'dark'));
  actions.appendChild(themeBtn);
  applyTheme();
}

function openSettings() {
  location.hash = '#/manage/general';
}

function expiryText() {
  const at = auth.expiresAt();
  if (!at) return '';
  const left = Math.max(0, Math.round((at - Date.now()) / 1000));
  return 'expires in ' + fmtDuration(left);
}

function signedInTitle() {
  const who = auth.user();
  const at = auth.expiresAt();
  return (who ? 'Signed in as ' + who : 'Signed in with a control token') + (at ? ', expires ' + new Date(at).toLocaleString() : '') + '. Opens Management, Sign-in.';
}

function refreshSignedIn() {
  const el = document.getElementById('signed-in-expiry');
  if (el) el.textContent = expiryText();
  const btn = document.getElementById('signed-in');
  if (btn) btn.title = signedInTitle();
}

// The default-password banner (password mode until the password is changed).
function renderBanner() {
  clear(bannerEl);
  const st = settings.instance.status;
  if (!st || !st.default_password || auth.needsSignIn()) return;
  bannerEl.appendChild(notice('warn',
    h('strong', null, 'This instance uses the default console password. '),
    'Anyone who reaches it can sign in with rustybin / rustybin. ',
    h('a', { href: '#/manage/signin', id: 'banner-change-password' }, 'Change it under Management, Sign-in'),
    ' or set RUSTYBIN_CONSOLE_PASSWORD.'));
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

// ── Sign-in screen (RUSTYBIN_CONTROL_AUTH = password | token | jwt) ──

let signinEl = null;

const REASONS = {
  missing: null,
  'signed-out': 'You signed out.',
  expired: 'Your session expired.',
  revoked: 'Your session was ended (signed out, or signed out everywhere).',
  invalid_credentials: 'Wrong user name or password.',
  rate_limited: 'Too many failed attempts: wait a moment and try again.',
  invalid_audience: 'The token was issued for another instance (audience mismatch).',
  invalid_signature: 'The token signature is not valid for this instance.',
  invalid_algorithm: 'The token uses an algorithm this instance does not accept.',
  not_yet_valid: 'The token is not valid yet (check the clock).',
  missing_claim: 'The token lacks a required claim (exp, aud).',
  malformed: 'The token is malformed.',
  invalid_token: 'The token was refused.',
  missing_token: 'The token did not reach the server.',
  unreachable: 'Rustybin could not be reached to check the token: try again.',
};

function renderSignIn() {
  if (!auth.needsSignIn()) {
    if (signinEl) { signinEl.remove(); signinEl = null; }
    app.removeAttribute('aria-hidden');
    return;
  }
  const reason = auth.getReason();
  const expired = reason === 'expired';
  const password = auth.passwordMode();
  const tokenInput = input({ type: 'password', placeholder: auth.mode === 'jwt' || password ? 'Access token or admin token' : 'Admin token', 'aria-label': 'Token', autocomplete: 'off', id: 'signin-token' });
  const err = h('div');
  const why = REASONS[reason] === undefined ? 'The token was refused (' + reason + ').' : REASONS[reason];
  const tokenHint = auth.mode === 'jwt' || password ? 'A control-plane access token (JWT) or RUSTYBIN_ADMIN_TOKEN. It is kept in this tab only.' : 'RUSTYBIN_ADMIN_TOKEN. It is kept in this tab only.';
  const tokenForm = h('form', { class: 'stack', id: 'signin-token-form' },
    field(auth.mode === 'jwt' || password ? 'Paste a token' : 'Paste the admin token', tokenInput, tokenHint),
    password ? null : err,
    h('div', { class: 'row' }, h('span', { class: 'grow' }), h('button', { class: 'btn primary', type: 'submit' }, password ? 'Use this token' : 'Sign in')));
  tokenForm.addEventListener('submit', (e) => {
    e.preventDefault();
    const v = tokenInput.value.trim();
    if (!v) {
      replace(err, notice('err', 'Enter a token.'));
      return;
    }
    auth.setToken(v);
  });
  // Password mode: the user name and password form first, the token as an alternative.
  const userInput = input({ placeholder: 'rustybin', 'aria-label': 'User name', autocomplete: 'username', id: 'signin-user', autocapitalize: 'off' });
  const passwordInput = input({ type: 'password', placeholder: 'password', 'aria-label': 'Password', autocomplete: 'current-password', id: 'signin-password' });
  const remember = checkbox('Remember me on this browser (12 hours at most)', false);
  remember.input.id = 'signin-remember';
  const submit = h('button', { class: 'btn primary', type: 'submit' }, 'Sign in');
  const passwordForm = h('form', { class: 'stack', id: 'signin-password-form' },
    field('User name', userInput),
    field('Password', passwordInput, 'rustybin / rustybin out of the box, until it is changed under Management, Sign-in.'),
    remember.el,
    err,
    h('div', { class: 'row' }, h('span', { class: 'grow' }), submit));
  passwordForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const u = userInput.value.trim();
    const p = passwordInput.value;
    if (!u || !p) {
      replace(err, notice('err', 'Enter the user name and the password.'));
      return;
    }
    submit.disabled = true;
    replace(err);
    const r = await auth.signIn(SERVER, u, p, remember.input.checked);
    submit.disabled = false;
    if (r.ok) return; // the token event verifies and resumes
    const text = r.reason === 'rate_limited' && r.retryAfter ? 'Too many failed attempts: try again in ' + fmtDuration(r.retryAfter) + '.' : (REASONS[r.reason] || r.message);
    replace(err, notice('err', text));
    passwordInput.value = '';
    passwordInput.focus();
  });
  const body = h('div', { class: 'stack' },
    h('div', { class: 'row nowrap' }, icon('shield'), h('h1', { id: 'signin-title' }, expired ? 'Session expired' : (password ? 'Sign in' : 'Sign in required'))),
    why ? notice(expired || reason === 'signed-out' ? 'warn' : 'err', why) : null,
    password
      ? h('p', null, 'This Rustybin instance protects its control plane and console with a user name and password. ',
        auth.backlink ? 'Open it again from your portal, or sign in below.' : 'Sign in below.')
      : h('p', null, 'This Rustybin instance protects its control plane and console data (RUSTYBIN_CONTROL_AUTH=' + auth.mode + '). ',
        auth.backlink ? 'Open the console again from your portal to get a fresh access token, or paste a token below.' : 'Paste a token below, or open the console with #token=<token> appended to its URL.'),
    auth.backlink ? h('a', { class: 'btn primary', href: auth.backlink, rel: 'noopener', id: 'signin-backlink' }, icon('external'), 'Open ' + backlinkLabel()) : null,
    password ? passwordForm : tokenForm,
    password ? h('details', { class: 'disclosure', id: 'signin-token-details' }, h('summary', null, 'Use a token instead'), h('div', { style: { paddingTop: '8px' } }, tokenForm)) : null,
    h('p', { class: 'muted small' }, 'The data plane stays open: ', h('a', { href: SERVER + '/' }, 'endpoint list'), ', ', h('a', { href: SERVER + '/docs' }, 'OpenAPI docs'), '.'));
  const el = h('div', { class: 'signin', id: 'signin', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'signin-title' },
    h('section', { class: 'card signin-card' }, h('div', { class: 'card-body' }, body)));
  if (signinEl) signinEl.replaceWith(el);
  else document.body.appendChild(el);
  signinEl = el;
  app.setAttribute('aria-hidden', 'true');
  document.title = (expired ? 'Session expired' : 'Sign in') + ' | ' + (auth.consoleTitle ? auth.consoleTitle + ' | ' : '') + 'Rustybin console';
  (password ? userInput : tokenInput).focus();
}

async function verifyAndResume() {
  // Check the new token once. A 401 already brought the sign-in screen back
  // with the reason; an unreachable server says so instead of doing nothing.
  const r = await send({ url: SERVER + '/_rustybin/status', session: false, timeout: 15000 });
  if (r.status === 401) return;
  if (r.error) {
    auth.markUnauthorized('unreachable');
    return;
  }
  // Signed in. A 403 is a valid token whose scope does not cover the overview
  // (an inspector token): the views it may read work, the others say why.
  renderSignIn();
  currentId = null;
  route();
  loadInstance().then(() => traffic.connect());
  if (r.status === 403) toast('Signed in with a limited token: some views need a wider scope.');
}

// ── Start ──────────────────────────────────────────────────────────

async function loadInstance() {
  if (auth.needsSignIn()) {
    renderMeta();
    renderBanner();
    return;
  }
  try {
    const st = await getJson('/_rustybin/status', { session: false });
    settings.instance.status = st;
    settings.instance.publicMode = !!st.public_mode;
    settings.instance.adminTokenConfigured = !!st.admin_token_configured;
    showFeatureViews(st);
    showAiMarks();
    const foot = document.getElementById('foot-version');
    if (foot) foot.textContent = 'v' + st.version + ' | up ' + fmtDuration(st.uptime_seconds);
    if (st.public_mode && !auth.boundKey() && !settings.get('session')) {
      const b = new Uint8Array(4);
      crypto.getRandomValues(b);
      settings.set('session', 'console-' + Array.from(b, (x) => x.toString(16).padStart(2, '0')).join(''));
    }
  } catch (e) {
    settings.instance.status = null;
    console.warn('status unavailable', e);
  }
  renderMeta();
  renderBanner();
  refreshSignedIn();
}

function start() {
  if (handleCallback()) return;
  auth.consumeFragment();
  visit.consumeFragment();
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
    visit.consumeFragment();
    route();
  });
  auth.onChange((kind) => {
    if (kind === 'token') {
      renderActions();
      verifyAndResume();
    } else {
      renderSignIn();
      renderActions();
      renderMeta();
      renderBanner();
    }
  });
  // A view that changed the instance (the password, say) asks for a fresh status.
  document.addEventListener('rustybin-status', () => loadInstance());
  // A view that changed the AI settings asks for a fresh assist status.
  document.addEventListener('rustybin-assist', () => assist.refresh());
  renderSignIn();
  document.addEventListener('keydown', (e) => {
    if (e.altKey && !e.ctrlKey && !e.metaKey && /^[1-9]$/.test(e.key)) {
      const views = VIEWS.filter((v) => v.id && !v.hidden);
      const v = views[Number(e.key) - 1];
      if (v) { location.hash = '#/' + v.id; e.preventDefault(); }
    }
    // Cmd+K / Ctrl+K: the assist bar.
    if ((e.metaKey || e.ctrlKey) && !e.altKey && (e.key === 'k' || e.key === 'K') && !auth.needsSignIn()) {
      e.preventDefault();
      assist.focusBar();
    }
  });
  assist.onChange(showAiMarks);
  route();
  loadInstance().then(() => { traffic.connect(); assist.refresh(); });
  // Refresh the health badge and footer now and then (cheap control-plane call).
  setInterval(loadInstance, 15000);
}

window.rustybinConsole = { settings, traffic };
start();
