// Management: the instance's settings and this browser's, in tabs down the
// left (General, Sign-in, Network; later steps add AI, Portal, Agents,
// Source control, Export). Instance settings come from GET /_rustybin/settings
// and save with PATCH (admin); the browser's (session, gateway URL, admin
// token, theme) stay in this browser as before. Sign-in changes the console
// user name and password (PUT /_rustybin/password, the current password
// required) and lists the sign-in sessions (GET /_rustybin/sessions).
// Routes: #/manage/<tab>.
import { h, replace, clear, card, field, input, select, notice, toast, icon, badge, spinner, kvgrid, fmtTime } from '../lib/dom.js';
import { SERVER, getJson, send, trimBase } from '../lib/http.js';
import * as settings from '../lib/settings.js';
import * as auth from '../lib/auth.js';
import * as assist from '../lib/assist.js';
import { agentPanel } from '../lib/agent.js';
import { settingsForm } from '../lib/settingsform.js';

// The rail. Later steps append entries here; each renders its own panel.
export const TABS = [
  { id: 'general', title: 'General', icon: 'settings', hint: 'Console title, this browser, your AI agent' },
  { id: 'signin', title: 'Sign-in', icon: 'lock', hint: 'The console user name and password, active sessions' },
  { id: 'ai', title: 'AI', icon: 'ai', hint: 'Your model provider and key for the AI features' },
  { id: 'network', title: 'Network', icon: 'globe', hint: 'Outbound connections and recent decisions' },
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
  } else {
    el.appendChild(card('Console', { hint: 'shown to everyone who opens it' }, loadError(error)));
  }
  el.appendChild(browserCard());
  el.appendChild(card(null, {}, h('details', { class: 'disclosure', id: 'agent-panel' },
    h('summary', null, 'Connect an AI agent (MCP)'),
    h('div', { style: { paddingTop: '8px' } }, agentPanel()))));
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
  const submit = h('button', { class: 'btn primary', type: 'submit', disabled: locked || !on }, 'Change the password');
  for (const c of [nameInput, current, next, confirm]) if (locked || !on) c.disabled = true;
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
    const r = await send({ method: 'PUT', url: SERVER + '/_rustybin/password', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), admin: true, session: false });
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
          const r = await send({ method: 'DELETE', url: SERVER + '/_rustybin/sessions/' + encodeURIComponent(s.id), admin: true, session: false });
          if (r.ok) { toast(s.id === d.current ? 'This session ended' : 'Session ended'); load(); } else toast('Could not end the session (' + r.status + ')', 'err');
        } }, 'End'))));
      const everywhere = h('button', { class: 'btn sm danger', type: 'button', id: 'sign-out-everywhere', onclick: async () => {
        if (!window.confirm('Sign out everywhere? Every sign-in token issued so far stops working, this one included.')) return;
        const r = await send({ method: 'DELETE', url: SERVER + '/_rustybin/sessions', admin: true, session: false });
        if (!r.ok) toast('Could not sign out everywhere (' + r.status + ')', 'err');
        // The server refuses this token from now on; the next call brings the sign-in screen.
        else auth.signOut();
      } }, icon('lock'), 'Sign out everywhere');
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
    return send({ method: 'PATCH', url: SERVER + '/_rustybin/settings', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ values }), admin: true, session: false })
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
    const r = await send({ method: 'POST', url: SERVER + '/_rustybin/assist/test', admin: true, session: false, timeout: 70000 });
    btn.disabled = false;
    const j = r.json || {};
    if (r.error || !r.ok) { replace(out, notice('err', r.error || j.error || ('The test failed (' + r.status + ')'))); return; }
    replace(out, notice('ok', 'The provider answered in ' + j.latency_ms + ' ms (' + j.provider + ' / ' + j.model + '): ', h('code', null, j.answer || ''), '.'));
    loadUsage();
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
      const testBtn = h('button', { class: 'btn sm', type: 'button', id: 'assist-test', disabled: !st.enabled }, icon('send'), 'Test the key');
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
    } catch (e) {
      replace(statusEl, loadError(e));
    }
  }

  if (doc) {
    const form = settingsForm(doc, 'assist', { saveLabel: 'Save AI settings', onSaved: (d) => { reload(d); document.dispatchEvent(new CustomEvent('rustybin-assist')); load(); } });
    el.appendChild(card('AI assist', { hint: 'the provider, the models, the test' }, statusEl));
    el.appendChild(card('Provider and keys', { hint: 'keys are sealed on the instance and never shown again' }, form.el));
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
    const leads = settingsForm(doc, 'leads', { saveLabel: 'Save leads webhook settings', onSaved: reload });
    el.appendChild(card('Leads webhook', { hint: 'self-service mocks' }, leads.el));
  } else {
    el.appendChild(card('Outbound connections', {}, loadError(error)));
  }
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
      const render = id === 'network' ? networkPanel : id === 'signin' ? signinPanel : id === 'ai' ? aiPanel : generalPanel;
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
        h('p', null, 'Configure this instance: what the console shows, who may sign in, where it may connect, and what this browser remembers. Instance settings override their environment variables and persist with RUSTYBIN_STATE_DIR.')),
      status));
    root.appendChild(h('div', { class: 'rail-layout' }, rail, panel));
    replace(panel, spinner('Loading settings'));

    getJson('/_rustybin/settings', { admin: true, session: false })
      .then((d) => { if (!alive) return; doc = d; renderStatus(); show(current); })
      .catch((e) => {
        if (!alive) return;
        doc = null;
        error = e;
        console.warn('settings unavailable', e);
        show(current);
      });

    const cleanup = () => { alive = false; };
    cleanup.onRoute = (r) => { if (TABS.some((t) => t.id === r) && r !== current) show(r); };
    cleanup.destroy = cleanup;
    return cleanup;
  },
};
