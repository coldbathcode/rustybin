// My account (the account menu): who you are, your roles and permissions,
// your display name and email (PATCH /_rustybin/me), your password (PUT
// /_rustybin/me/password, the current one required) and your sign-in
// sessions (GET and DELETE /_rustybin/me/sessions, DELETE
// /_rustybin/me/sessions/{id}). Every signed-in user reaches it; a token
// that is not a user's (the admin token, a control JWT) is told so. The same
// on every surface: rustybin me and passwd.
import { h, replace, card, field, input, notice, toast, icon, badge, spinner, kvgrid } from '../lib/dom.js';
import { SERVER, getJson, send } from '../lib/http.js';
import * as auth from '../lib/auth.js';

/** A date and time in the browser's locale (sign-ins, expiries days ahead). */
function when(iso) {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? String(iso || '') : d.toLocaleString();
}

async function call(method, path, body) {
  const r = await send({
    control: true, session: false, method, url: SERVER + path,
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (r.error) throw new Error(r.error);
  if (!r.ok) {
    const e = new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
    e.status = r.status;
    e.reason = r.json && r.json.reason;
    throw e;
  }
  return r.json;
}

function profileCard(me, reload) {
  const display = input({ id: 'me-display', value: me.display_name, maxlength: 100, 'aria-label': 'Display name' });
  const email = input({ id: 'me-email', type: 'email', value: me.email || '', 'aria-label': 'Email' });
  const err = h('div');
  const save = h('button', { class: 'btn primary', type: 'submit', id: 'me-save' }, 'Save');
  const form = h('form', { class: 'stack', 'aria-label': 'Your profile' },
    kvgrid([
      ['Sign-in name', h('span', { class: 'mono', id: 'me-name' }, me.name)],
      ['Roles', h('div', { class: 'row', style: { gap: '4px', flexWrap: 'wrap' }, id: 'me-roles' }, me.roles.map((r) => badge(r)))],
      ['Status', badge(me.status, me.status === 'active' ? 'ok' : 'warn')],
      ['Last sign-in', me.last_sign_in ? when(me.last_sign_in) : 'now'],
    ]),
    field('Display name', display),
    field('Email', email, 'Optional; never logged. Empty clears it.'),
    err,
    h('div', { class: 'row' }, h('span', { class: 'grow muted small' }, 'Your roles are changed by whoever manages users.'), save));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    replace(err);
    try {
      await call('PATCH', '/_rustybin/me', { display_name: display.value.trim(), email: email.value.trim() || null });
      toast('Saved');
      reload();
    } catch (ex) {
      replace(err, notice('err', ex.message));
    }
  });
  return card('Your account', { hint: 'who you are on this instance' }, form);
}

function permissionsCard(me) {
  return card('Your permissions', { hint: me.permissions.length + ' permission(s), from your roles' },
    h('div', { class: 'row', style: { gap: '4px', flexWrap: 'wrap' }, id: 'me-permissions' }, me.permissions.map((p) => h('span', { class: 'badge outline mono' }, p))));
}

function passwordCard(me) {
  const current = input({ type: 'password', id: 'me-current', autocomplete: 'current-password', 'aria-label': 'Current password' });
  const next = input({ type: 'password', id: 'me-next', autocomplete: 'new-password', placeholder: 'at least 8 characters', 'aria-label': 'New password' });
  const confirm = input({ type: 'password', id: 'me-confirm', autocomplete: 'new-password', 'aria-label': 'Confirm the new password' });
  const err = h('div');
  const locked = !!(me.password && me.password.locked);
  const submit = h('button', { class: 'btn primary', type: 'submit', id: 'me-password-save', disabled: locked }, 'Change your password');
  const form = h('form', { class: 'stack', 'aria-label': 'Change your password' },
    locked ? notice('', icon('lock'), 'RUSTYBIN_SETTINGS_LOCK holds the first admin\'s password: the environment sets it.') : null,
    field('Current password', current),
    field('New password', next, 'Kept as an argon2id hash in ' + ((me.password && me.password.kept_in) || 'the user store') + '. Not the default.'),
    field('Confirm', confirm),
    err,
    h('div', { class: 'row' }, h('span', { class: 'grow muted small' }, 'Your other sessions end; this one continues.'), submit));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (next.value !== confirm.value) { replace(err, notice('err', 'The new password and its confirmation differ.')); return; }
    if (next.value.length < 8) { replace(err, notice('err', 'The new password needs at least 8 characters.')); return; }
    submit.disabled = true;
    replace(err);
    try {
      const v = await call('PUT', '/_rustybin/me/password', { current_password: current.value, new_password: next.value });
      if (v.token) auth.setToken(v.token, !!localStorage.getItem('rustybin.console.rememberedToken'));
      current.value = ''; next.value = ''; confirm.value = '';
      toast('Password changed');
      document.dispatchEvent(new CustomEvent('rustybin-status'));
    } catch (ex) {
      replace(err, notice('err', ex.message));
    } finally {
      submit.disabled = locked;
    }
  });
  return card('Your password', { hint: 'the current one is required' }, form);
}

function sessionsCard() {
  const el = h('div', null, spinner('Loading your sessions'));
  async function load() {
    try {
      const d = await call('GET', '/_rustybin/me/sessions');
      const rows = d.sessions.map((s) => h('tr', null,
        h('td', { class: 'mono small' }, s.ip || ''),
        h('td', { class: 'small' }, when(s.issued_at), s.id === d.current ? h('span', null, ' ', badge('this session', 'info')) : null),
        h('td', { class: 'small' }, when(s.last_used)),
        h('td', { class: 'small' }, when(s.expires_at)),
        h('td', null, h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'End session ' + s.id, onclick: async () => {
          try { await call('DELETE', '/_rustybin/me/sessions/' + encodeURIComponent(s.id)); toast('Session ended'); load(); } catch (ex) { toast(ex.message, 'err'); }
        } }, 'End'))));
      replace(el,
        h('div', { class: 'row', style: { marginBottom: '10px' } },
          h('span', { class: 'muted small grow' }, d.count + ' session(s) of yours.'),
          h('button', { class: 'btn sm danger', type: 'button', id: 'me-sign-out-everywhere', onclick: async () => {
            if (!window.confirm('Sign yourself out everywhere? This session ends too.')) return;
            try { await call('DELETE', '/_rustybin/me/sessions'); auth.signOut(); } catch (ex) { toast(ex.message, 'err'); }
          } }, icon('lock'), 'Sign out everywhere')),
        rows.length ? h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'me-sessions' },
          h('thead', null, h('tr', null, h('th', null, 'Address'), h('th', null, 'Signed in'), h('th', null, 'Last used'), h('th', null, 'Expires'), h('th', null, ''))),
          h('tbody', null, rows))) : notice('', 'No session is listed.'));
    } catch (ex) {
      replace(el, notice('err', ex.message));
    }
  }
  load();
  return card('Your sessions', { hint: 'where you are signed in' }, el);
}

export default {
  id: 'account',
  mount(root) {
    let alive = true;
    const body = h('div', { class: 'stack' }, spinner('Loading your account'));
    root.append(h('div', { class: 'view-head' }, h('div', { class: 'grow' }, h('h1', null, 'My account'),
      h('p', null, 'Your profile, your password and where you are signed in. Nobody but you ever sees or types your password.'))), body);
    async function load() {
      try {
        const me = await getJson('/_rustybin/me', { session: false });
        if (!alive) return;
        replace(body, profileCard(me, load), permissionsCard(me), passwordCard(me), sessionsCard());
      } catch (ex) {
        if (!alive) return;
        const msg = ex && ex.message ? ex.message : String(ex);
        replace(body, /^40[04]/.test(msg)
          ? notice('', 'This credential is not a user\'s (the admin token, a control JWT, or an open control plane): it has no account here. Management, Sign-in shows the sessions; users sign in with the password sign-in.')
          : notice('err', 'Your account could not be loaded: ' + msg));
      }
    }
    load();
    return () => { alive = false; };
  },
};
