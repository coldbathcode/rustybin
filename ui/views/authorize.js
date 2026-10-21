// Consent page of the management MCP server's OAuth sign-in
// (/ui/#/authorize/<id>): an agent asked to sign in; the person in front of
// the console approves or denies with the console's own credential, and the
// grant is capped by what that credential may do. Not in the navigation.
import { h, replace, card, notice, icon, spinner, badge } from '../lib/dom.js';
import { SERVER, send } from '../lib/http.js';
import * as auth from '../lib/auth.js';

const CHOICES = [
  ['admin', 'Full access', 'Read everything and change this instance: create, deploy and delete mocks, apply configuration, health toggles.'],
  ['console', 'Read only', 'Read the instance, its mocks and captured traffic, send test requests; no changes.'],
  ['inspector', 'Traffic only', 'Read captured traffic (request inspector, mock calls, bins); nothing else.'],
];

/** Where the browser goes: the host of a web URL, else the URI scheme. */
function destination(uri) {
  try {
    const u = new URL(uri);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.host : u.protocol.replace(/:$/, '');
  } catch {
    return uri;
  }
}

export default {
  id: 'authorize',
  mount(root, { rest }) {
    let alive = true;
    const id = String(rest || '').trim();
    const body = h('div', { class: 'stack' });
    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', { id: 'authorize-title' }, 'Sign in an AI agent'),
        h('p', null, 'An agent asked to manage this Rustybin through its management MCP server (/_rustybin/mcp).'))));
    root.appendChild(h('div', { style: { maxWidth: '680px' } }, body));
    const url = SERVER + '/_rustybin/oauth/requests/' + encodeURIComponent(id);

    function fail(kind, ...content) {
      replace(body, notice(kind, ...content));
    }

    function explain(r) {
      const msg = r.json && r.json.error ? String(r.json.error) : r.error || ('HTTP ' + r.status);
      if (r.status === 404) {
        return fail('warn', 'This sign-in request is unknown or has expired (requests last 10 minutes and are answered once). Start the sign-in again from your agent.');
      }
      if (r.status === 403) return fail('err', 'Your console credential cannot grant this access: ', msg);
      if (r.status === 401) return fail('warn', 'Sign in to the console to answer this request.');
      return fail('err', msg);
    }

    async function load() {
      if (!id || !/^[A-Za-z0-9_-]{1,128}$/.test(id)) {
        return fail('err', 'This address does not name a sign-in request. Start the sign-in again from your agent.');
      }
      if (auth.needsSignIn()) {
        return fail('info', 'Sign in to the console to answer this request.');
      }
      replace(body, spinner('Loading the sign-in request'));
      const r = await send({ url, session: false });
      if (!alive) return;
      if (!r.ok || !r.json) return explain(r);
      render(r.json);
    }

    function render(req) {
      const grantable = Array.isArray(req.scopes_grantable) ? req.scopes_grantable : [];
      const requested = Array.isArray(req.scopes_requested) ? req.scopes_requested : [];
      const name = String(req.client_name || 'MCP client');
      const kind = req.redirect_kind;
      const dest = destination(String(req.redirect_uri || ''));
      const err = h('div');
      const destination_ = kind === 'loopback'
        ? notice('info', 'After you answer, the browser returns to the agent on this computer (', h('span', { class: 'mono' }, dest), ').')
        : notice('warn', h('strong', null, 'The answer goes to ', h('span', { class: 'mono' }, dest), '. '),
          kind === 'app'
            ? 'That is an application on this device, not a local address. Approve only if you started this sign-in from that application.'
            : 'That is a website, not a local address. Approve only if you started this sign-in from that site and trust it.');

      let choice = CHOICES.find(([v]) => grantable.includes(v));
      const cards = h('div', { class: 'radio-cards', role: 'radiogroup', 'aria-labelledby': 'authorize-access' });
      for (const [value, label, text] of CHOICES) {
        if (!requested.includes(value)) continue;
        const allowed = grantable.includes(value);
        const radio = h('input', { type: 'radio', name: 'authorize-scope', value, checked: choice && choice[0] === value, disabled: !allowed });
        radio.addEventListener('change', () => { choice = CHOICES.find(([v]) => v === value); });
        cards.appendChild(h('label', { class: 'radio-card', 'data-scope': value, style: allowed ? null : { opacity: '0.55', cursor: 'not-allowed' } }, radio,
          h('span', { class: 'stack-sm', style: { gap: '2px' } },
            h('strong', null, label, h('span', { class: 'mono small muted', style: { marginLeft: '8px' } }, value)),
            h('span', { class: 'small muted' }, allowed ? text : text + ' Your console credential cannot grant this.'))));
      }

      const approve = h('button', { class: 'btn primary', type: 'button', id: 'authorize-approve', disabled: !choice }, icon('check'), 'Approve');
      const deny = h('button', { class: 'btn', type: 'button', id: 'authorize-deny' }, 'Deny');
      approve.addEventListener('click', () => answer('approve'));
      deny.addEventListener('click', () => answer('deny'));

      async function answer(decision) {
        approve.disabled = true;
        deny.disabled = true;
        replace(err);
        const payload = { decision };
        if (decision === 'approve') payload.scope = choice[0];
        const r = await send({ url, method: 'POST', session: false, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload) });
        if (!alive) return;
        if (!r.ok || !r.json || !r.json.redirect_to) {
          approve.disabled = !choice;
          deny.disabled = false;
          if (r.status === 404 || r.status === 401) return explain(r);
          replace(err, notice('err', r.json && r.json.error ? String(r.json.error) : r.error || ('HTTP ' + r.status)));
          return;
        }
        const done = decision === 'approve'
          ? notice('ok', 'Approved (', choice[1].toLowerCase(), '). Returning to the agent.')
          : notice('info', 'Denied. The agent is told the request was refused.');
        replace(body, done, r.json.redirect_kind === 'web' ? null
          : h('p', { class: 'muted', id: 'authorize-done' }, 'You can close this tab and return to your agent.'));
        window.location.assign(r.json.redirect_to);
      }

      const shield = icon('shield');
      Object.assign(shield.style, { width: '22px', height: '22px', flex: 'none', color: 'var(--accent)', marginTop: '2px' });
      const expires = Number(req.expires_in) > 0 ? Math.ceil(Number(req.expires_in) / 60) : null;
      replace(body,
        card(null, null, h('div', { class: 'stack' },
          h('div', { class: 'row nowrap', style: { alignItems: 'flex-start' } }, shield,
            h('h2', { class: 'grow', id: 'authorize-client', style: { minWidth: '0', overflowWrap: 'anywhere' } }, name),
            h('span', { style: { flex: 'none' } }, badge('client ' + String(req.client || ''), 'outline'))),
          h('p', null, 'This name was chosen by the agent when it registered: check that it is the agent you just connected.'),
          destination_,
          h('h3', { id: 'authorize-access' }, 'Access to grant'),
          grantable.length ? cards : notice('err', 'The credential this console signed in with cannot grant any of the access the agent asked for (',
            requested.join(', '), '). Sign in to the console with a token that has the needed scope, or deny.'),
          err,
          h('div', { class: 'row' }, h('span', { class: 'grow small muted' }, expires ? 'This request expires in about ' + expires + ' min.' : null), deny, approve))));
      const first = cards.querySelector('input:checked') || deny;
      first.focus();
    }

    load();
    return () => { alive = false; };
  },
};
