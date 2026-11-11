// Management: the instance's settings and this browser's, in tabs down the
// left (General, Sign-in, Users and roles, AI, Network, Portal, Export,
// Source control, Checkpoints, Audit, Diagnostics). Users and roles lives in
// ./manage/users.js, Audit (the audit log, its live tail and its
// retention settings) in ./manage/audit.js.
// Export and Source control live in ./manage/export.js and ./manage/git.js
// (the whole instance as rustybin.yaml, and its git repository). Portal
// publishes items to the dev portal at
// /portal (GET /_rustybin/portal, PUT and DELETE /_rustybin/portal/items/{id})
// and shows its insights. Instance settings come from GET /_rustybin/settings
// and save with PATCH (admin); the browser's (session, gateway URL, admin
// token, theme) stay in this browser as before. Sign-in changes the console
// user name and password (PUT /_rustybin/password, the current password
// required) and lists the sign-in sessions (GET /_rustybin/sessions).
// Above the tabs, the secrets AI agents asked to have set
// (../lib/secretrequests.js): typed here or declined, never sent to the agent.
// Routes: #/manage/<tab>.
import { h, replace, clear, card, field, input, select, notice, toast, icon, badge, spinner, kvgrid, fmtTime } from '../lib/dom.js';
import { SERVER, getJson, send, trimBase } from '../lib/http.js';
import * as settings from '../lib/settings.js';
import * as auth from '../lib/auth.js';
import * as assist from '../lib/assist.js';
import { settingsForm } from '../lib/settingsform.js';
import { secretRequests } from '../lib/secretrequests.js';
import * as portal from '../lib/portal.js';
import { exportPanel } from './manage/export.js';
import { gitPanel } from './manage/git.js';
import { checkpointsPanel } from './manage/checkpoints.js';
import { usersPanel } from './manage/users.js';
import { auditPanel } from './manage/audit.js';
import * as perms from '../lib/perms.js';

// The rail. Later steps append entries here; each renders its own panel.
export const TABS = [
  { id: 'general', title: 'General', icon: 'settings', hint: 'Console title, this browser, your AI agent' },
  { id: 'signin', title: 'Sign-in', icon: 'lock', hint: 'The console user name and password, single sign-on, active sessions' },
  { id: 'users', title: 'Users and roles', icon: 'user', hint: 'Invite people, give them roles, disable or remove them; custom roles' },
  { id: 'ai', title: 'AI', icon: 'ai', hint: 'Your model provider and key for the AI features' },
  { id: 'network', title: 'Network', icon: 'globe', hint: 'Outbound connections and recent decisions' },
  { id: 'portal', title: 'Portal', icon: 'explorer', hint: 'The dev portal: branding, access, published items, insights' },
  { id: 'export', title: 'Export', icon: 'file', hint: 'The whole instance as rustybin.yaml: export, diff, apply a document' },
  { id: 'git', title: 'Source control', icon: 'history', hint: 'Commit the configuration to a git repository, pull and apply it' },
  { id: 'checkpoints', title: 'Checkpoints', icon: 'history', hint: 'Save the mocks and health a demo starts from, restore them before a run' },
  { id: 'audit', title: 'Audit', icon: 'file', hint: 'Who changed what, every sign-in and every refusal; how long the log is kept' },
  { id: 'diagnostics', title: 'Diagnostics', icon: 'shield', hint: 'What is wrong with this instance and how to fix it' },
];

function loadError(e) {
  const msg = e && e.message ? e.message : String(e);
  if (/^40[13]/.test(msg)) return notice('warn', icon('lock'), h('div', { class: 'grow' }, 'Instance settings need the admin token (or a control token with the admin scope). ', 'Set it under "This browser" below.'));
  return notice('err', 'Settings could not be loaded: ' + msg);
}

// ── General ──

function browserCard() {
  const session = input({ value: settings.get('session'), placeholder: 'e.g. demo-acme', id: 'browser-session' });
  const gateway = input({ value: settings.get('gatewayUrl'), placeholder: 'https://gateway.example.com/rustybin', type: 'url', id: 'browser-gateway' });
  const admin = input({ value: settings.get('adminToken'), placeholder: 'only when RUSTYBIN_ADMIN_TOKEN is set', type: 'password', id: 'browser-admin', autocomplete: 'off' });
  const theme = select([['system', 'Follow the system'], ['light', 'Light'], ['dark', 'Dark']], settings.get('theme'), { id: 'browser-theme' });
  const err = h('div');
  const exp = auth.expiresAt();
  const signedIn = auth.required() && auth.getToken()
    ? h('div', { class: 'notice' }, icon('shield'), h('div', { class: 'grow' },
      'Signed in to the control plane (RUSTYBIN_CONTROL_AUTH=' + auth.mode + ')',
      auth.scopes() ? ', scope ' + auth.scopes().join(' ') : '',
      exp ? ', expires ' + new Date(exp).toLocaleTimeString() : '', '.'),
    h('button', { class: 'btn sm', type: 'button', id: 'sign-out', onclick: () => auth.signOut(SERVER) }, 'Sign out'))
    : null;
  const form = h('form', { class: 'stack', 'aria-label': 'This browser' },
    signedIn,
    field('Session (X-Rustybin-Session)', session, 'Tags the requests this console sends, scopes per-client state (flaky counters, bins, tasks). Required to see traffic in public mode. Letters, digits and . _ : - only.'),
    field('Gateway base URL', gateway, 'Views offer "via gateway" to send their requests through this URL instead of directly to Rustybin. Cross-origin calls need CORS on the gateway.'),
    field('Admin token', admin, (settings.instance.adminTokenConfigured ? 'This instance requires it for instance settings, health toggles and clearing all captured requests.' : 'Not required by this instance.') + ' Kept in this tab only.'),
    field('Theme', theme),
    err,
    h('div', { class: 'row' }, h('span', { class: 'muted small' }, 'Stored in this browser, never on the instance.'), h('span', { class: 'grow' }),
      h('button', { class: 'btn primary', type: 'submit' }, 'Save browser settings')));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const sv = session.value.trim();
    if (sv && !/^[A-Za-z0-9._:-]{1,128}$/.test(sv)) {
      replace(err, notice('err', 'The session may only contain letters, digits and . _ : - (at most 128 characters).'));
      return;
    }
    const gv = trimBase(gateway.value);
    if (gv && !/^https?:\/\/[^\s]+$/.test(gv)) {
      replace(err, notice('err', 'The gateway URL must start with http:// or https://'));
      return;
    }
    replace(err);
    settings.set('session', sv);
    settings.set('gatewayUrl', gv);
    settings.set('adminToken', admin.value.trim());
    settings.set('theme', theme.value);
    toast('Browser settings saved');
  });
  return card('This browser', { hint: 'session, gateway, admin token, theme' }, form);
}

function generalPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  if (doc) {
    const form = settingsForm(doc, 'console', { saveLabel: 'Save console settings', onSaved: reload });
    el.appendChild(card('Console', { hint: 'shown to everyone who opens it' }, form.el));
    const mocks = settingsForm(doc, 'mocks', { saveLabel: 'Save mock settings', onSaved: reload });
    el.appendChild(card('Mock edits', { hint: 'conflict protection' }, mocks.el));
    const attacks = settingsForm(doc, 'mcp', { saveLabel: 'Save MCP settings', onSaved: reload });
    el.appendChild(card('MCP attack servers', { hint: 'for MCP security demos; always off in public mode' }, attacks.el));
  } else {
    el.appendChild(card('Console', { hint: 'shown to everyone who opens it' }, loadError(error)));
  }
  el.appendChild(browserCard());
  el.appendChild(card(null, {}, h('p', { class: 'small', style: { margin: 0 }, id: 'agent-panel' }, icon('terminal'), ' Connect an AI agent (MCP), see what agents did and limit or revoke them in ',
    h('a', { href: '#/agents' }, 'AI agents'), '.')));
  return el;
}

// ── Sign-in ──

function settingOf(doc, key) {
  return ((doc && doc.settings) || []).find((s) => s.key === key) || null;
}

function sourceText(setting) {
  if (!setting) return '';
  const src = setting.source;
  if (src === 'stored') return 'saved in this instance';
  if (src === 'env') return 'from the environment (' + setting.env + ')';
  return 'the default';
}

function statusCard(doc, status) {
  const user = settingOf(doc, 'console.user');
  const hash = settingOf(doc, 'console.password_hash');
  const hashState = hash && hash.value ? hash.value : {};
  const on = auth.passwordMode();
  const passwordText = !on ? 'not used in this mode'
    : hashState.set ? 'a stored hash (' + (hashState.source === 'env' ? 'RUSTYBIN_CONSOLE_PASSWORD_HASH' : 'changed in the console') + ')'
      : status && status.default_password ? h('span', { class: 'badge warn' }, 'the default: rustybin') : 'RUSTYBIN_CONSOLE_PASSWORD';
  const pairs = [
    ['Control-plane auth', h('span', { class: 'mono' }, auth.mode) ],
    ['User name', user ? h('span', null, h('span', { class: 'mono' }, user.value || 'rustybin'), h('span', { class: 'muted small' }, ' ' + sourceText(user))) : 'rustybin'],
    ['Password', passwordText],
    ['Signed in as', auth.user() ? h('span', { class: 'mono' }, auth.user()) : (auth.getToken() ? 'a control token (not the password sign-in)' : 'nobody: the control plane is open')],
  ];
  if (auth.expiresAt()) pairs.push(['Session expires', new Date(auth.expiresAt()).toLocaleString()]);
  const items = [kvgrid(pairs)];
  if (!on) {
    items.unshift(notice('', 'The password sign-in is off: RUSTYBIN_CONTROL_AUTH is ', h('code', null, auth.mode), '. The settings below apply once the instance runs in password mode (the default).'));
  } else if (status && status.default_password) {
    items.unshift(notice('warn', h('strong', null, 'Default password. '), 'Anyone who reaches this instance can sign in with rustybin / rustybin: change it below, or set RUSTYBIN_CONSOLE_PASSWORD.'));
  }
  if (doc && doc.locked && (user && user.locked)) items.push(notice('', icon('lock'), h('div', { class: 'grow' }, 'RUSTYBIN_SETTINGS_LOCK is on: the environment is authoritative for the user name and password (RUSTYBIN_CONSOLE_USER, RUSTYBIN_CONSOLE_PASSWORD_HASH).')));
  if (on) items.push(h('p', { class: 'small muted', style: { margin: 0 } }, 'The user name and password here are the first admin\'s (the instance\'s sign-in settings). Your own profile, password and sessions: ',
    h('a', { href: '#/account' }, 'My account'), '. Everyone else: ', h('a', { href: '#/manage/users' }, 'Users and roles'), '.'));
  return card('Sign-in', { hint: 'who may open the console and the control plane' }, h('div', { class: 'stack' }, ...items));
}

function passwordCard(doc, status, onChanged) {
  const user = settingOf(doc, 'console.user');
  const locked = !!(user && user.locked);
  const on = auth.passwordMode();
  const nameInput = input({ value: (user && user.value) || 'rustybin', 'aria-label': 'console.user', autocomplete: 'username', id: 'signin-new-user', maxlength: 64 });
  const current = input({ type: 'password', 'aria-label': 'Current password', autocomplete: 'current-password', id: 'signin-current' });
  const next = input({ type: 'password', 'aria-label': 'console.password_hash', autocomplete: 'new-password', id: 'signin-next', placeholder: 'at least 8 characters' });
  const confirm = input({ type: 'password', 'aria-label': 'Confirm the new password', autocomplete: 'new-password', id: 'signin-confirm' });
  const err = h('div');
  const submit = perms.guard(h('button', { class: 'btn primary', type: 'submit', disabled: locked || !on }, 'Change the password'), 'users.manage');
  for (const c of [nameInput, current, next, confirm]) if (locked || !on || !perms.can('users.manage')) c.disabled = true;
  const form = h('form', { class: 'stack', 'aria-label': 'Change the password' },
    field('User name', nameInput, 'Letters, digits and . _ @ - only. ' + (user ? 'Now ' + sourceText(user) + '.' : '')),
    field('Current password', current, 'Required to change anything here, even for an administrator.'),
    field('New password', next, 'Stored as an argon2id hash in the instance settings (console.password_hash); never shown again. Not the default.'),
    field('Confirm', confirm),
    err,
    h('div', { class: 'row' },
      h('span', { class: 'muted small' }, 'Every other sign-in session ends; this one continues with a fresh token.'),
      h('span', { class: 'grow' }), submit));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (next.value !== confirm.value) {
      replace(err, notice('err', 'The new password and its confirmation differ.'));
      confirm.focus();
      return;
    }
    if (next.value.length < 8) {
      replace(err, notice('err', 'The new password needs at least 8 characters.'));
      next.focus();
      return;
    }
    submit.disabled = true;
    replace(err);
    const body = { current_password: current.value, new_password: next.value };
    const name = nameInput.value.trim();
    if (name && name !== ((user && user.value) || 'rustybin')) body.username = name;
    const r = await send({ control: true, method: 'PUT', url: SERVER + '/_rustybin/password', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), admin: true, session: false });
    submit.disabled = false;
    if (r.error || !r.ok) {
      const j = r.json || {};
      replace(err, notice('err', r.error || j.error || ('The change failed (' + r.status + ').'), j.problems ? ' ' + j.problems.join('; ') : ''));
      if (r.status === 403) { current.value = ''; current.focus(); }
      return;
    }
    if (r.json && r.json.token) auth.setToken(r.json.token, !!localStorage.getItem('rustybin.console.rememberedToken'));
    current.value = ''; next.value = ''; confirm.value = '';
    toast('Password changed' + (r.json && r.json.signed_out > 1 ? ', ' + (r.json.signed_out - 1) + ' other session(s) ended' : ''));
    onChanged(r.json);
  });
  return card('Change the password', { hint: 'the current password is required' }, form);
}

function sessionsCard(reloadRef) {
  const el = h('div', null, spinner('Loading sessions'));
  async function load() {
    try {
      const d = await getJson('/_rustybin/sessions', { admin: true, session: false });
      const rows = (d.sessions || []).map((s) => h('tr', { class: s.id === d.current ? 'current' : '' },
        h('td', null, h('span', { class: 'mono' }, s.user), s.id === d.current ? h('span', null, ' ', badge('this session', 'info')) : null),
        h('td', { class: 'mono small' }, s.ip || ''),
        h('td', { class: 'small', title: s.issued_at }, fmtTime(s.issued_at)),
        h('td', { class: 'small', title: s.last_used }, fmtTime(s.last_used)),
        h('td', { class: 'small', title: s.expires_at }, fmtTime(s.expires_at)),
        h('td', null, h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'End session ' + s.id, onclick: async () => {
          const r = await send({ control: true, method: 'DELETE', url: SERVER + '/_rustybin/sessions/' + encodeURIComponent(s.id), admin: true, session: false });
          if (r.ok) { toast(s.id === d.current ? 'This session ended' : 'Session ended'); load(); } else toast('Could not end the session (' + r.status + ')', 'err');
        } }, 'End'))));
      const everywhere = perms.guard(h('button', { class: 'btn sm danger', type: 'button', id: 'sign-out-everywhere', onclick: async () => {
        if (!window.confirm('Sign out everywhere? Every sign-in token issued so far stops working, this one included.')) return;
        const r = await send({ control: true, method: 'DELETE', url: SERVER + '/_rustybin/sessions', admin: true, session: false });
        if (!r.ok) toast('Could not sign out everywhere (' + r.status + ')', 'err');
        // The server refuses this token from now on; the next call brings the sign-in screen.
        else auth.signOut();
      } }, icon('lock'), 'Sign out everywhere'), 'users.manage');
      replace(el,
        h('div', { class: 'row', style: { marginBottom: '10px' } },
          h('span', { class: 'muted small grow' }, d.count + ' session(s) since the last restart or sign-out-everywhere (generation ' + d.generation + '; at most ' + d.capacity + ' are listed).'),
          h('button', { class: 'btn sm ghost', type: 'button', onclick: load }, icon('refresh'), 'Refresh'),
          everywhere),
        rows.length ? h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'sessions-table' },
          h('thead', null, h('tr', null, h('th', null, 'User'), h('th', null, 'Address'), h('th', null, 'Signed in'), h('th', null, 'Last used'), h('th', null, 'Expires'), h('th', null, ''))),
          h('tbody', null, rows))) : notice('', 'No sign-in session is listed.'));
    } catch (e) {
      replace(el, loadError(e));
    }
  }
  load();
  reloadRef.sessions = load;
  return card('Sessions', { hint: 'sign-in tokens issued by this instance' }, el);
}

/** Single sign-on: the sso.* settings and a Test that reads the provider now. */
function ssoCard(doc, reload) {
  const out = h('div');
  const testBtn = h('button', { class: 'btn', type: 'button', id: 'sso-test' }, icon('check'), 'Test');
  perms.guard(testBtn, 'settings.write');
  testBtn.addEventListener('click', async () => {
    testBtn.disabled = true;
    replace(out, spinner('Reading the provider\'s discovery document and keys'));
    const r = await send({ control: true, method: 'POST', url: SERVER + '/_rustybin/sso/test', admin: true, session: false });
    testBtn.disabled = false;
    const v = r.json || {};
    if (r.error || !r.ok) {
      replace(out, notice('err', r.error || v.error || ('The test failed (' + r.status + ')')));
      return;
    }
    const rows = [
      ['Redirect URI to register', h('code', null, v.redirect_uri || '')],
      ['Issuer', v.issuer || ''],
      ['Client', (v.client_id || '') + (v.client_secret && v.client_secret.set ? ' (confidential: a secret is set)' : ' (public: PKCE only)')],
      ['Scopes', (v.scopes || []).join(' ')],
    ];
    if (v.discovery) rows.push(['Token endpoint', v.discovery.token_endpoint], ['Keys', String((v.keys || []).length) + ' (' + (v.keys || []).map((k) => k.alg || k.kty).join(', ') + ')']);
    replace(out, h('div', { class: 'stack' },
      v.ok ? notice('ok', icon('check'), 'The provider answers: single sign-on is ready.') : notice('err', v.error || 'Not ready'),
      v.unknown_roles && v.unknown_roles.length ? notice('warn', 'The role map names roles this instance does not have: ' + v.unknown_roles.join(', ')) : null,
      kvgrid(rows)));
  });
  const form = doc ? settingsForm(doc, 'sso', { saveLabel: 'Save single sign-on', onSaved: (d) => { reload(d); document.dispatchEvent(new CustomEvent('rustybin-status')); } }) : null;
  return card('Single sign-on', { hint: 'any OpenID Connect provider: its groups decide the roles' }, h('div', { class: 'stack' },
    h('p', { class: 'muted small', style: { margin: 0 } }, 'Register ', h('code', null, SERVER + '/_rustybin/sso/callback'), ' as the client\'s redirect URI at the provider. The role map gives each group its roles (group=role role); a person\'s roles are recomputed at every sign-in. Turning the password sign-in off leaves single sign-on and the admin token.'),
    form ? form.el : null,
    h('div', { class: 'row' }, testBtn, h('span', { class: 'muted small' }, 'POST /_rustybin/sso/test: discovery and the keys, fetched now through the outbound policy (purpose sso).')),
    out));
}

function signinPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  const ref = {};
  const onChanged = async () => {
    // The hash is now stored and the default-password flag is gone: refresh
    // the settings document, the status (banner, chip) and the sessions.
    try { reload(await getJson('/_rustybin/settings', { admin: true, session: false })); } catch { /* keep the old one */ }
    document.dispatchEvent(new CustomEvent('rustybin-status'));
    if (ref.sessions) ref.sessions();
    rerender();
  };
  function rerender() {
    clear(el);
    el.appendChild(statusCard(doc, settings.instance.status));
    if (doc) el.appendChild(passwordCard(doc, settings.instance.status, onChanged));
    else el.appendChild(card('Change the password', {}, loadError(error)));
    if (auth.passwordMode()) el.appendChild(ssoCard(doc, reload));
    if (auth.passwordMode()) el.appendChild(sessionsCard(ref));
  }
  const reloadDoc = reload;
  reload = (d) => { if (d) doc = d; reloadDoc(d); };
  rerender();
  return el;
}

// ── AI ──

function priceText(m) {
  const p = m.price_per_million;
  if (!p) return 'see the provider\'s price list';
  if (p.input === 0 && p.output === 0) return 'free';
  return '$' + p.input + ' in, $' + p.output + ' out per 1M tokens';
}

function aiPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  const statusEl = h('div', null, spinner('Reading the AI assist status'));
  const usageEl = h('div');
  let st = null;

  function pick(values) {
    return send({ control: true, method: 'PATCH', url: SERVER + '/_rustybin/settings', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ values }), admin: true, session: false })
      .then((r) => {
        if (r.error || !r.ok) { toast(r.error || (r.json && r.json.error) || ('Save failed (' + r.status + ')'), 'err'); return; }
        toast('Saved ' + Object.keys(values).join(', '));
        reload(r.json);
        document.dispatchEvent(new CustomEvent('rustybin-assist'));
        load();
      });
  }

  function modelsTable() {
    const rows = [];
    for (const p of st.providers || []) {
      for (const m of p.models || []) {
        const current = st.provider === p.id && (st.model === m.id || st.fast_model === m.id);
        rows.push(h('tr', { class: current ? 'current' : '' },
          h('td', null, p.id),
          h('td', { class: 'mono small' }, m.id, st.provider === p.id && st.model === m.id ? h('span', null, ' ', badge('model', 'info')) : null, st.provider === p.id && st.fast_model === m.id ? h('span', null, ' ', badge('fast', 'info')) : null),
          h('td', null, badge(m.tier, m.tier === 'fast' ? 'ok' : 'violet')),
          h('td', { class: 'small' }, priceText(m)),
          h('td', { class: 'small muted' }, m.note),
          h('td', null, h('div', { class: 'row nowrap', style: { gap: '4px' } },
            h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'Use ' + m.id + ' as the model', onclick: () => pick({ 'assist.provider': p.id, 'assist.model': m.id }) }, 'Use'),
            h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'Use ' + m.id + ' as the fast model', onclick: () => pick({ 'assist.provider': p.id, 'assist.fast_model': m.id }) }, 'Fast')))));
      }
    }
    return h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'assist-models' },
      h('thead', null, h('tr', null, h('th', null, 'Provider'), h('th', null, 'Model'), h('th', null, 'Tier'), h('th', null, 'Price'), h('th', null, ''), h('th', null, ''))),
      h('tbody', null, rows)));
  }

  async function test(btn, out) {
    btn.disabled = true;
    replace(out, spinner('Calling the provider'));
    const r = await send({ control: true, method: 'POST', url: SERVER + '/_rustybin/assist/test', admin: true, session: false, timeout: 70000 });
    btn.disabled = false;
    const j = r.json || {};
    if (r.error || !r.ok) { replace(out, notice('err', r.error || j.error || ('The test failed (' + r.status + ')'))); return; }
    replace(out, notice('ok', 'The provider answered in ' + j.latency_ms + ' ms (' + j.provider + ' / ' + j.model + '): ', h('code', null, j.answer || ''), '.'));
    loadUsage();
  }

  // The configured provider's models (GET /_rustybin/assist/models: the list
  // last loaded from the provider merged with the curated table) and the job
  // each runs; Load asks the provider with the stored key (POST, admin).
  const catalogEl = h('div', { id: 'assist-catalog' }, spinner('Reading the models'));
  // The provider's models as a list each job field offers (a model can
  // still be typed: a compatible endpoint takes any id).
  const modelOptions = h('datalist', { id: 'assist-model-options' });
  let jobsFormEl = null;
  function offerModels(models) {
    replace(modelOptions, ...(models || []).map((m) => h('option', { value: m.id, label: m.tier + (m.name ? ', ' + m.name : '') })));
    if (!jobsFormEl) return;
    for (const inp of jobsFormEl.querySelectorAll('input[aria-label^="assist.job."]')) {
      inp.setAttribute('list', 'assist-model-options');
      inp.setAttribute('placeholder', 'Pick a model, or empty for the tier default');
    }
  }

  function jobsTable(jobs) {
    return h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'assist-jobs' },
      h('thead', null, h('tr', null, h('th', null, 'Job'), h('th', null, 'Tier'), h('th', null, 'Model'), h('th', null, 'From'))),
      h('tbody', null, (jobs || []).map((j) => h('tr', null,
        h('td', null, j.title, h('div', { class: 'muted small mono' }, j.task)),
        h('td', null, badge(j.tier, j.tier === 'fast' ? 'ok' : 'violet')),
        h('td', { class: 'mono small' }, j.model || '(none)'),
        h('td', { class: 'small' }, j.ignored
          ? h('span', { class: 'inline-icon' }, badge('ignored', 'warn'), ' ', h('code', null, j.ignored), ' is not a model of this provider: the job runs on its tier default')
          : (j.source === 'setting' ? 'assist.job setting' : 'tier default')))))));
  }

  function catalogTable(c) {
    const rows = (c.models || []).map((m) => h('tr', { class: (m.default_for || []).length ? 'current' : '' },
      h('td', { class: 'mono small' }, m.id, m.name ? h('div', { class: 'muted small' }, m.name) : null),
      h('td', null, badge(m.tier, m.tier === 'fast' ? 'ok' : 'violet'), m.reasoning ? h('span', null, ' ', badge('reasons', 'info')) : null),
      h('td', { class: 'small' }, m.context ? String(m.context) : ''),
      h('td', { class: 'small' }, priceText(m)),
      h('td', { class: 'small' }, m.listed ? (m.curated ? 'listed, curated' : 'listed') : h('span', { class: 'muted' }, 'curated, not listed')),
      h('td', { class: 'small' }, [...(m.default_for || []).map((d) => d === 'model' ? 'model' : 'fast model'), ...(m.jobs || [])].join(', ')),
      h('td', null, h('div', { class: 'row nowrap', style: { gap: '4px' } },
        h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'Use ' + m.id + ' as the model', onclick: () => pick({ 'assist.model': m.id }) }, 'Use'),
        h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'Use ' + m.id + ' as the fast model', onclick: () => pick({ 'assist.fast_model': m.id }) }, 'Fast')))));
    return h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'assist-provider-models' },
      h('thead', null, h('tr', null, h('th', null, 'Model'), h('th', null, 'Tier'), h('th', null, 'Context'), h('th', null, 'Price'), h('th', null, 'Source'), h('th', null, 'Runs'), h('th', null, ''))),
      h('tbody', null, rows)));
  }

  function renderCatalog(c) {
    const loadBtn = perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'assist-load-models', disabled: !c.provider }, icon('refresh'), 'Load the provider\'s models'), 'settings.write');
    const out = h('div');
    loadBtn.addEventListener('click', async () => {
      loadBtn.disabled = true;
      replace(out, spinner('Asking ' + c.provider + ' for its models'));
      const r = await send({ control: true, method: 'POST', url: SERVER + '/_rustybin/assist/models', admin: true, session: false, timeout: 70000 });
      loadBtn.disabled = false;
      const j = r.json || {};
      if (r.error || !r.ok) { replace(out, notice('err', r.error || j.error || ('Loading failed (' + r.status + ')'))); return; }
      clear(out);
      renderCatalog(j);
    });
    const ignored = (c.jobs || []).filter((j) => j.ignored);
    offerModels(c.models);
    replace(catalogEl,
      c.provider
        ? h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } }, loadBtn,
          h('span', { class: 'muted small' }, c.loaded
            ? 'Loaded from ' + c.provider + ' ' + fmtTime(c.loaded.at) + ': ' + c.loaded.kept + ' model(s)' + (c.loaded.listed > c.loaded.kept ? ' of ' + c.loaded.listed : '') + '.'
            : 'The curated models of ' + c.provider + '. Load the list to see every model your key can call.'))
        : notice('', 'Pick a provider first: the models and jobs follow it.'),
      out,
      ignored.length ? notice('warn', ignored.length + ' job setting(s) name a model ' + c.provider + ' does not offer; those jobs run on their tier default until you change them.') : null,
      c.provider ? catalogTable(c) : null,
      h('h3', { class: 'section-title', style: { marginTop: '14px' } }, 'Jobs'),
      jobsTable(c.jobs));
  }

  async function loadCatalog() {
    try {
      renderCatalog(await getJson('/_rustybin/assist/models', { session: false }));
    } catch (e) {
      replace(catalogEl, loadError(e));
    }
  }

  async function loadUsage() {
    try {
      const u = (await getJson('/_rustybin/assist/usage', { admin: true, session: false })).total || {};
      replace(usageEl, kvgrid([
        ['Calls', String(u.calls || 0) + (u.errors ? ' (' + u.errors + ' failed)' : '')],
        ['Tokens', (u.input_tokens || 0) + ' in, ' + (u.output_tokens || 0) + ' out'],
        ['Estimated cost', typeof u.estimated_cost_usd === 'number' ? '$' + u.estimated_cost_usd.toFixed(4) + (u.unpriced_calls ? ' (some calls have no list price)' : '') : 'unknown (no list price for the models used)'],
      ]));
    } catch (e) {
      replace(usageEl, loadError(e));
    }
  }

  async function load() {
    try {
      st = await getJson('/_rustybin/assist', { session: false });
      const testBtn = perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'assist-test', disabled: !st.enabled }, icon('send'), 'Test the key'), 'settings.write');
      const testOut = h('div');
      testBtn.addEventListener('click', () => test(testBtn, testOut));
      const mockBtn = h('button', { class: 'btn sm ghost', type: 'button', id: 'assist-use-mock', onclick: () => pick({ 'assist.provider': 'mock' }) }, 'Use the built-in mock LLM');
      replace(statusEl,
        st.enabled
          ? notice('ok', icon('check'), h('div', { class: 'grow' }, h('strong', null, 'AI assist is on: '), st.provider + ' / ' + st.model + (st.fast_model && st.fast_model !== st.model ? ' (fast: ' + st.fast_model + ')' : ''), st.provider === 'mock' ? '. The built-in provider answers deterministically; nothing leaves this instance.' : '.'))
          : notice('warn', h('div', { class: 'grow' }, h('strong', null, 'AI assist is off: '), st.reason || 'no provider', '. Pick a provider and add its key below, or use the built-in mock LLM for a demo.')),
        h('div', { class: 'row', style: { marginTop: '10px' } }, testBtn, mockBtn, h('span', { class: 'muted small' }, 'Caps: ' + st.caps.calls_per_minute + ' calls per minute per caller, ' + st.caps.max_output_tokens + ' output tokens per answer.')),
        testOut,
        h('h3', { class: 'section-title', style: { marginTop: '14px' } }, 'Curated models'),
        modelsTable());
      loadUsage();
      loadCatalog();
    } catch (e) {
      replace(statusEl, loadError(e));
    }
  }

  if (doc) {
    const form = settingsForm(doc, 'assist', { saveLabel: 'Save AI settings', onSaved: (d) => { reload(d); document.dispatchEvent(new CustomEvent('rustybin-assist')); load(); } });
    el.appendChild(card('AI assist', { hint: 'the provider, the models, the test' }, statusEl));
    el.appendChild(card('Provider and keys', { hint: 'keys are sealed on the instance and never shown again' }, form.el));
    const jobsForm = settingsForm(doc, 'assist_jobs', { saveLabel: 'Save job models', onSaved: (d) => { reload(d); document.dispatchEvent(new CustomEvent('rustybin-assist')); load(); } });
    jobsFormEl = jobsForm.el;
    el.appendChild(card('Models and jobs', { hint: 'GET and POST /_rustybin/assist/models' }, catalogEl,
      h('h3', { class: 'section-title', style: { marginTop: '14px' } }, 'Choose a model per job'),
      h('p', { class: 'muted small', style: { margin: '0 0 8px' } }, 'Each field offers the provider\'s models (load the list above for every model your key can call). Empty: the job runs on its tier default (the fast model or the model). A model the provider does not offer is ignored, and flagged in the jobs table.'),
      modelOptions,
      jobsForm.el));
    el.appendChild(card('Usage since start', { hint: 'GET /_rustybin/assist/usage' }, usageEl));
    el.appendChild(card('Privacy', {}, h('p', { class: 'small', style: { margin: 0 } }, (st && st.privacy) || 'Prompts carry only what the task names and go to the configured provider through the outbound policy. Keys never leave the instance. Inputs that contain a stored secret or the admin token are refused. Every answer shows exactly what was sent.')));
    load();
  } else {
    el.appendChild(card('AI assist', {}, loadError(error)));
  }
  return el;
}

// ── Network ──

function purposesTable(policy) {
  const rows = (policy.purposes || []).map((p) => h('tr', null,
    h('td', null, h('div', null, p.title), h('div', { class: 'muted small' }, p.configured_by)),
    h('td', { class: 'mono small' }, p.any ? badge('any host', 'warn') : (p.allow && p.allow.length ? p.allow.join(', ') : h('span', { class: 'muted' }, 'the global list only'))),
    h('td', null, p.loopback ? badge('loopback ok', 'info') : '')));
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
    h('thead', null, h('tr', null, h('th', null, 'Feature'), h('th', null, 'Its own rule'), h('th', null, ''))),
    h('tbody', null, rows)));
}

function decisionsTable(policy) {
  const d = policy.decisions || [];
  if (!d.length) return notice('', 'No outbound connection has been attempted since the instance started.');
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
    h('thead', null, h('tr', null, h('th', null, 'When'), h('th', null, 'Feature'), h('th', null, 'Destination'), h('th', null, 'Decision'))),
    h('tbody', null, d.map((x) => h('tr', null,
      h('td', { class: 'mono small', title: x.at }, fmtTime(x.at)),
      h('td', null, x.purpose),
      h('td', { class: 'mono small' }, x.origin, x.addrs && x.addrs.length ? h('div', { class: 'muted' }, x.addrs.join(', ')) : null),
      h('td', null, x.allowed ? badge('allowed', 'ok') : h('div', null, badge('refused', 'err'), h('div', { class: 'small muted' }, x.reason))))))));
}

// GET /_rustybin/egress/check: a dry check, nothing is called.
function egressCheck() {
  const url = input({ id: 'egress-check-url', placeholder: 'https://api.example.com/', 'aria-label': 'URL to check' });
  const purpose = input({ id: 'egress-check-purpose', placeholder: 'mcp_relay', 'aria-label': 'Purpose' });
  const out = h('div', { id: 'egress-check-result' });
  const form = h('form', { class: 'stack' },
    field('URL', url),
    field('Purpose', purpose, 'The feature that would call it, as in the table above. Empty: the MCP relay.'),
    h('div', { class: 'row' }, h('button', { class: 'btn', type: 'submit' }, 'Check')),
    out);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const q = new URLSearchParams({ url: url.value.trim() });
    if (purpose.value.trim()) q.set('purpose', purpose.value.trim());
    try {
      const r = await getJson('/_rustybin/egress/check?' + q, { admin: true, session: false });
      replace(out, r.allowed
        ? notice('ok', (r.origin || r.url) + ' would be allowed for ' + r.purpose + '. Its addresses are checked again when a call is made.')
        : notice('err', (r.origin || r.url) + ' would be refused for ' + r.purpose + ': ' + (r.reasons || []).join('; ')));
    } catch (err) {
      replace(out, loadError(err));
    }
  });
  return form;
}

function networkPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  const policyEl = h('div', null, spinner('Reading the policy'));
  async function loadPolicy() {
    try {
      const policy = await getJson('/_rustybin/egress', { admin: true, session: false });
      replace(policyEl,
        policy.warnings && policy.warnings.length ? notice('warn', 'Ignored entries: ' + policy.warnings.join('; ')) : null,
        kvgrid([
          ['Global allowlist', policy.allow.length ? h('span', { class: 'mono' }, policy.allow.join(', ')) : 'empty (only each feature\'s own rule applies)'],
          ['Loopback', policy.allow_loopback ? 'allowed (' + policy.allow_loopback_source + ')' : 'refused unless a feature\'s rule allows it'],
          ['Always refused', policy.always_refused.join(', ')],
          ['Public mode', policy.public_mode ? 'on: no feature may reach any host' : 'off'],
        ]),
        h('h3', { class: 'section-title' }, 'Allowed by feature'),
        purposesTable(policy),
        h('div', { class: 'row', style: { marginTop: '12px' } }, h('h3', { class: 'section-title grow', style: { margin: 0 } }, 'Recent decisions'),
          h('button', { class: 'btn sm ghost', type: 'button', onclick: loadPolicy }, icon('refresh'), 'Refresh')),
        decisionsTable(policy));
    } catch (e) {
      replace(policyEl, loadError(e));
    }
  }
  if (doc) {
    const form = settingsForm(doc, 'egress', { saveLabel: 'Save outbound settings', onSaved: (d) => { reload(d); loadPolicy(); } });
    el.appendChild(card('Outbound connections', { hint: 'what this instance may reach' }, form.el));
    loadPolicy();
    el.appendChild(card('Policy as the instance sees it', { hint: 'the merged list, each feature, the last 100 decisions' }, policyEl));
    el.appendChild(card('Check a URL', { hint: 'what the policy would say, without calling it' }, egressCheck()));
    const proxy = settingsForm(doc, 'proxy', { saveLabel: 'Save proxy settings', onSaved: reload });
    el.appendChild(card('Behind a proxy', { hint: 'which header names the client (RUSTYBIN_TRUST_FORWARD)' }, proxy.el));
    const leads = settingsForm(doc, 'leads', { saveLabel: 'Save leads webhook settings', onSaved: reload });
    el.appendChild(card('Leads webhook', { hint: 'self-service mocks' }, leads.el));
    const notify = settingsForm(doc, 'notify', { saveLabel: 'Save notice settings', onSaved: reload });
    el.appendChild(card('Test schedule notices', { hint: 'the webhooks of scheduled saved tests (purpose notify)', actions: h('a', { class: 'btn sm', href: '#/tests' }, 'Open the schedules') }, notify.el));
    const registry = settingsForm(doc, 'registry', { saveLabel: 'Save registry settings', onSaved: reload });
    el.appendChild(card('MCP registry', { hint: 'the mock registry\'s scenario, registries to browse', actions: h('a', { class: 'btn sm', href: '#/registry' }, 'Open the registry view') }, registry.el));
  } else {
    el.appendChild(card('Outbound connections', {}, loadError(error)));
  }
  return el;
}

// ── Diagnostics ──

// GET /_rustybin/diagnostics (admin): every check with its status, what was
// seen and the fix. Not readiness, not the demo /health toggle.
function diagnosticsPanel() {
  const el = h('div', { class: 'stack' });
  const body = h('div', { id: 'diagnostics' }, spinner('Running the checks'));
  const kind = { ok: 'ok', warn: 'warn', fail: 'err' };
  async function load() {
    replace(body, spinner('Running the checks'));
    try {
      const r = await getJson('/_rustybin/diagnostics', { admin: true, session: false });
      const rows = (r.checks || []).map((c) => h('tr', { 'data-check': c.id },
        h('td', null, badge(c.status, kind[c.status] || 'outline')),
        h('td', { class: 'mono small' }, c.id),
        h('td', { class: 'small' }, c.message, c.fix ? h('div', { class: 'muted small' }, 'Fix: ', c.fix) : null)));
      replace(body,
        r.status === 'ok' ? notice('ok', icon('check'), ' Every check is ok.')
          : notice(r.status === 'fail' ? 'err' : 'warn', r.status === 'fail' ? 'A check failed: see the fix on its row.' : 'Some checks want attention.'),
        h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'diagnostics-checks' },
          h('thead', null, h('tr', null, h('th', null, 'Status'), h('th', null, 'Check'), h('th', null, 'What was seen'))),
          h('tbody', null, rows))),
        h('p', { class: 'muted small' }, 'Checked ' + fmtTime(r.checked_at) + '. The same checks: rustybin doctor, the diagnose_instance tool, GET /_rustybin/diagnostics.'));
    } catch (e) {
      replace(body, loadError(e));
    }
  }
  el.appendChild(card('Diagnostics', { hint: 'GET /_rustybin/diagnostics', actions: h('button', { class: 'btn sm', type: 'button', id: 'diagnostics-run', onclick: load }, icon('refresh'), 'Run again') }, body));
  load();
  return el;
}

// ── Portal ──

const KIND_TITLES = { api: 'Mock APIs', mcp: 'MCP servers', llm: 'Mock LLM', agent: 'A2A agents', tools: 'HTTP tools', registry: 'MCP registry' };

function portalPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  const statusEl = h('div', null, spinner('Reading the portal'));
  const itemsEl = h('div');
  const insightsEl = h('div');
  const selected = new Set();
  let st = null;

  function statusCard() {
    const url = portal.portalUrl();
    const mode = st.mode === 'auto' ? 'auto (on once an item is published)' : st.mode;
    replace(statusEl,
      st.enabled
        ? notice('ok', icon('check'), h('div', { class: 'grow' }, h('strong', null, 'The portal is on: '), st.counts.shown + ' of ' + st.counts.published + ' published items are shown. ', h('a', { href: url, target: '_blank', rel: 'noopener', id: 'portal-preview' }, icon('external'), ' Preview the portal')))
        : notice('warn', h('div', { class: 'grow' }, h('strong', null, 'The portal is off: '), st.mode === 'off' ? 'portal.enabled is off.' : 'nothing is published yet. Publish an item below.')),
      kvgrid([
        ['URL', h('span', { class: 'mono' }, url)],
        ['Mode', mode],
        ['Who may read it', st.auth === 'open' ? 'anyone who reaches it' : st.auth === 'password' ? 'signed-in users (the console sign-in or any control credential)' : 'holders of an API key'],
        ['Base URL for testers', st.base_url ? h('span', { class: 'mono' }, st.base_url) : 'the URL the portal is opened with'],
        ['Closed items', h('span', { id: 'portal-closed-count' }, (st.counts.closed || 0) + ' closed' + (st.auth === 'password' ? ' (portal.auth password closes every item)' : ''))],
        ['Readers', h('span', { id: 'portal-readers' }, st.readers
          ? st.readers.users + ' user(s) read closed items, ' + st.readers.portal_only + ' with a portal-only login (Users and roles: Invite a portal reader)'
          : 'portal logins need the password sign-in; a bearer control credential with portal.read reads closed items')],
        ['Visitors may publish', st.visitors ? 'yes (self-service visitors, their own mocks)' : 'no'],
        ['Saved', st.persisted ? 'in the state directory (portal.json)' : 'in memory: publishing is lost on a restart (set RUSTYBIN_STATE_DIR)'],
      ]));
  }

  async function bulk(publish) {
    const ids = Array.from(selected);
    const failed = [];
    for (const id of ids) {
      const r = publish ? await portal.publish(id, {}) : await portal.unpublish(id);
      if (!r.ok && !(r.status === 404 && !publish)) failed.push(id + ': ' + ((r.json && r.json.error) || r.status));
    }
    selected.clear();
    if (failed.length) toast(failed.join('; '), 'err');
    else toast((publish ? 'Published ' : 'Unpublished ') + ids.length + ' item' + (ids.length === 1 ? '' : 's'));
    load();
  }

  function itemsCard() {
    const items = st.items || [];
    const bar = h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } });
    const renderBar = () => replace(bar, h('span', { class: 'muted small grow' }, selected.size ? selected.size + ' selected' : 'Select items to publish several at once.'),
      perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'portal-bulk-publish', disabled: !selected.size, onclick: () => bulk(true) }, 'Publish selected'), 'portal.publish'),
      perms.guard(h('button', { class: 'btn sm ghost', type: 'button', disabled: !selected.size, onclick: () => bulk(false) }, 'Unpublish selected'), 'portal.publish'));
    renderBar();
    const groups = [];
    for (const kind of Object.keys(KIND_TITLES)) {
      const list = items.filter((i) => i.kind === kind);
      if (!list.length) continue;
      groups.push(h('h3', { class: 'section-title' }, KIND_TITLES[kind]));
      groups.push(h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
        h('thead', null, h('tr', null, h('th', { style: { width: '28px' } }, ''), h('th', null, 'Item'), h('th', null, 'Portal'), h('th', null, 'Views (7 days)'), h('th', null, ''))),
        h('tbody', null, ...list.map((it) => {
          const p = it.published;
          const cb = h('input', { type: 'checkbox', 'aria-label': 'Select ' + it.id, checked: selected.has(it.id), disabled: !!(p && p.source === 'file') });
          cb.addEventListener('change', () => { if (cb.checked) selected.add(it.id); else selected.delete(it.id); renderBar(); });
          const state = !p ? badge('not published', '')
            : it.hidden ? h('span', null, badge('hidden', 'warn'), h('div', { class: 'tiny muted' }, it.hidden))
              : badge(p.source === 'file' ? 'published (file)' : 'published', 'ok');
          const lock = p && p.access === 'closed'
            ? h('span', { class: 'badge warn', 'data-closed': it.id, title: 'Closed: read by everyone, used only by signed-in portal readers' }, icon('lock'), 'closed')
            : null;
          const warning = it.warning ? h('div', { class: 'tiny', style: { color: 'var(--warn, inherit)' } }, it.warning) : null;
          const edit = h('button', { class: 'btn sm', type: 'button', 'data-portal-item': it.id }, p ? 'Edit' : 'Publish');
          edit.addEventListener('click', () => portal.openPublishDialog(it, { onDone: () => load() }));
          return h('tr', { 'data-item': it.id },
            h('td', null, cb),
            h('td', null, h('div', null, (p && p.title) || it.title), h('div', { class: 'tiny muted mono' }, it.id + (it.kind === 'api' && it.state !== 'live' ? ' (' + it.state + ')' : '')),
              p && p.visitor ? badge('visitor', 'outline') : null),
            h('td', null, h('div', { class: 'row', style: { gap: '4px', flexWrap: 'wrap' } }, state, lock), warning),
            h('td', { class: 'mono small' }, it.insights ? String(it.insights.views_7d) : ''),
            h('td', { class: 'right' }, edit));
        })))));
    }
    replace(itemsEl, card('Items', { hint: st.counts.published + ' published, at most ' + st.counts.max }, h('div', { class: 'stack-sm' }, bar, ...groups)));
  }

  function insightsCard() {
    const rows = (st.items || []).filter((i) => i.published && i.insights);
    if (!rows.length) { replace(insightsEl, card('Insights', {}, h('p', { class: 'muted small', style: { margin: 0 } }, 'Nothing published yet.'))); return; }
    replace(insightsEl, card('Insights', { hint: 'counts only, no visitor identifiers' }, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'portal-insights' },
      h('thead', null, h('tr', null, h('th', null, 'Item'), h('th', null, '7 days'), h('th', null, '30 days'), h('th', null, 'Downloads'), h('th', null, 'Try it'))),
      h('tbody', null, ...rows.map((i) => h('tr', null,
        h('td', null, (i.published && i.published.title) || i.title),
        h('td', { class: 'mono small' }, String(i.insights.views_7d)),
        h('td', { class: 'mono small' }, String(i.insights.views_30d)),
        h('td', { class: 'mono small', title: Object.entries(i.insights.downloads || {}).map(([k, v]) => k + ' ' + v).join(', ') }, String(i.insights.downloads_total)),
        h('td', { class: 'mono small' }, String(i.insights.tries)))))))));
  }

  async function load() {
    try {
      st = await portal.load();
      statusCard();
      itemsCard();
      insightsCard();
    } catch (e) {
      replace(statusEl, loadError(e));
    }
  }

  el.appendChild(card('Dev portal', { hint: 'what testers see at /portal', actions: h('button', { class: 'btn sm ghost', type: 'button', onclick: () => load() }, icon('refresh'), 'Refresh') }, statusEl));
  if (doc) {
    const form = settingsForm(doc, 'portal', { saveLabel: 'Save portal settings', onSaved: (d) => { reload(d); load(); } });
    el.appendChild(card('Branding and access', { hint: 'title, introduction, colour, who may read it' }, form.el));
  } else {
    el.appendChild(card('Branding and access', {}, loadError(error)));
  }
  el.append(itemsEl, insightsEl);
  load();
  return el;
}

// ── The view ──

export default {
  id: 'manage',
  mount(root, { rest }) {
    let alive = true;
    let doc;
    let error;
    let current = TABS.some((t) => t.id === rest) ? rest : 'general';
    const rail = h('nav', { class: 'rail', role: 'tablist', 'aria-orientation': 'vertical', 'aria-label': 'Management sections' });
    const panel = h('div', { class: 'rail-panel', role: 'tabpanel' });
    const buttons = new Map();
    const status = h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } });

    function renderStatus() {
      clear(status);
      if (!doc) return;
      status.appendChild(doc.persisted ? badge('persisted', 'ok') : badge('in memory', 'warn'));
      if (doc.locked) status.appendChild(badge('locked by the environment', 'info'));
      if (doc.unreadable_secrets && doc.unreadable_secrets.length) status.appendChild(badge(doc.unreadable_secrets.length + ' secret(s) unreadable', 'err'));
      if (doc.updated_at) status.appendChild(h('span', { class: 'muted small' }, 'changed ' + fmtTime(doc.updated_at)));
    }

    function show(id) {
      current = id;
      for (const [tid, b] of buttons) {
        b.setAttribute('aria-selected', String(tid === id));
        b.tabIndex = tid === id ? 0 : -1;
      }
      if (location.hash !== '#/manage/' + id) history.replaceState(null, '', '#/manage/' + id);
      const render = id === 'users' ? usersPanel : id === 'audit' ? auditPanel : id === 'checkpoints' ? checkpointsPanel : id === 'diagnostics' ? diagnosticsPanel : id === 'network' ? networkPanel : id === 'signin' ? signinPanel : id === 'ai' ? aiPanel : id === 'portal' ? portalPanel : id === 'export' ? exportPanel : id === 'git' ? gitPanel : generalPanel;
      replace(panel, render(doc, (d) => { if (d) { doc = d; renderStatus(); } }, error));
    }

    for (const t of TABS) {
      const b = h('button', { type: 'button', role: 'tab', 'aria-selected': 'false', 'aria-controls': 'manage-panel', id: 'manage-tab-' + t.id, title: t.hint },
        icon(t.icon), h('span', null, t.title));
      b.addEventListener('click', () => show(t.id));
      b.addEventListener('keydown', (e) => {
        if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
        const idx = TABS.findIndex((x) => x.id === current);
        const next = TABS[(idx + (e.key === 'ArrowDown' ? 1 : TABS.length - 1)) % TABS.length];
        show(next.id);
        buttons.get(next.id).focus();
        e.preventDefault();
      });
      buttons.set(t.id, b);
      rail.appendChild(b);
    }
    panel.id = 'manage-panel';

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Management'),
        h('p', null, 'Configure this instance: what the console shows, who may sign in, where it may connect, what this browser remembers, and the whole instance as rustybin.yaml in a git repository. Instance settings override their environment variables and persist with RUSTYBIN_STATE_DIR.')),
      status));
    // Secrets an AI agent asked to have set: above every tab, since a
    // secret of any group may be asked for. Setting one reloads the forms.
    const loadSettings = () => getJson('/_rustybin/settings', { admin: true, session: false })
      .then((d) => { if (!alive) return; doc = d; renderStatus(); show(current); })
      .catch((e) => {
        if (!alive) return;
        doc = null;
        error = e;
        console.warn('settings unavailable', e);
        show(current);
      });
    const requests = secretRequests({ onSet: loadSettings });
    root.appendChild(requests.el);
    root.appendChild(h('div', { class: 'rail-layout' }, rail, panel));
    replace(panel, spinner('Loading settings'));

    loadSettings();

    const cleanup = () => { alive = false; requests.destroy(); };
    cleanup.onRoute = (r) => { if (TABS.some((t) => t.id === r) && r !== current) show(r); };
    cleanup.destroy = cleanup;
    return cleanup;
  },
};
