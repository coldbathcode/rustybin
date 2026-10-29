// Management: the instance's settings and this browser's, in tabs down the
// left (General, Network; later steps add Sign-in, AI, Portal, Agents,
// Source control, Export). Instance settings come from GET /_rustybin/settings
// and save with PATCH (admin); the browser's (session, gateway URL, admin
// token, theme) stay in this browser as before. Routes: #/manage/<tab>.
import { h, replace, clear, card, field, input, select, notice, toast, icon, badge, spinner, kvgrid, fmtTime } from '../lib/dom.js';
import { SERVER, getJson, trimBase } from '../lib/http.js';
import * as settings from '../lib/settings.js';
import * as auth from '../lib/auth.js';
import { agentPanel } from '../lib/agent.js';
import { settingsForm } from '../lib/settingsform.js';

// The rail. Later steps append entries here; each renders its own panel.
export const TABS = [
  { id: 'general', title: 'General', icon: 'settings', hint: 'Console title, this browser, your AI agent' },
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
    h('button', { class: 'btn sm', type: 'button', id: 'sign-out', onclick: () => auth.signOut() }, 'Sign out'))
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
      const render = id === 'network' ? networkPanel : generalPanel;
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
        h('p', null, 'Configure this instance: what the console shows, where it may connect, and what this browser remembers. Instance settings override their environment variables and persist with RUSTYBIN_STATE_DIR.')),
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
