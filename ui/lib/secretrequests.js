// Secrets an AI agent asked to have set (GET /_rustybin/secret-requests):
// a card on the Management view listing each pending request with what the
// instance knows (the setting, the agent's id, the user it acts for, its
// credential, when the request expires) set apart from what the agent
// claims (the name it gave itself and its reason). The value is typed here
// and sent with POST /_rustybin/secret-requests/{id}/fulfil, or the request
// is declined; it never reaches the agent. Everything an agent wrote is put
// in the page as text (createTextNode), never as markup.
import { h, replace, card, input, notice, toast, icon, badge, kvgrid } from './dom.js';
import { SERVER, getJson, send } from './http.js';
import * as perms from './perms.js';

const PATH = '/_rustybin/secret-requests';

function minutes(seconds) {
  const s = Math.max(0, Number(seconds) || 0);
  if (s < 60) return s + ' s';
  return Math.ceil(s / 60) + ' min';
}

function requestRow(r, reload) {
  const setting = r.setting || {};
  const agent = r.agent || {};
  const key = String(setting.key || '');
  const lacking = perms.missing(setting.needs || []);
  const err = h('div');
  const pw = input({ type: 'password', 'aria-label': 'Value for ' + key, placeholder: 'the value, typed by you', autocomplete: 'new-password' });
  const setBtn = h('button', { class: 'btn primary sm', type: 'submit', 'aria-label': 'Set ' + key }, 'Set the secret');
  const declineBtn = h('button', { class: 'btn sm', type: 'button', 'aria-label': 'Decline ' + key }, 'Decline');
  if (setting.locked) {
    pw.disabled = true;
    setBtn.disabled = true;
    setBtn.title = 'Locked by RUSTYBIN_SETTINGS_LOCK: the environment is authoritative.';
  } else if (lacking) {
    pw.disabled = true;
    perms.guard(setBtn, lacking);
  }
  perms.guard(declineBtn, 'secrets.write');

  async function answer(what, body) {
    setBtn.disabled = true;
    declineBtn.disabled = true;
    const res = await send({ control: true, method: 'POST', url: SERVER + PATH + '/' + encodeURIComponent(r.id) + '/' + what, headers: { 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined, admin: true, session: false });
    pw.value = '';
    if (res.error || !res.ok) {
      const j = res.json || {};
      const detail = (j.problems || j.locked || []).join('; ');
      replace(err, notice('err', res.error || j.error || ('Failed (' + res.status + ')'), detail ? ' ' + detail : ''));
      if (res.status === 410 || res.status === 404) reload();
      else { setBtn.disabled = !!setting.locked || !!lacking; declineBtn.disabled = false; }
      return;
    }
    toast(what === 'fulfil' ? key + ' is set. The agent reads that it was done, never the value.' : 'Declined: nothing was set.');
    reload(what === 'fulfil');
  }

  const form = h('form', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' }, 'aria-label': 'Secret request ' + r.id }, h('div', { class: 'grow', style: { minWidth: '220px' } }, pw), setBtn, declineBtn);
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    if (!pw.value.trim()) { replace(err, notice('err', 'Type the value, or decline the request.')); return; }
    replace(err);
    answer('fulfil', { value: pw.value });
  });
  declineBtn.addEventListener('click', () => answer('decline'));

  const owner = agent.owner
    ? h('span', null, h('strong', null, String(agent.owner_label || '')), agent.owner.status && agent.owner.status !== 'active' ? h('span', { class: 'badge warn' }, String(agent.owner.status)) : null)
    : h('span', null, String(agent.owner_label || 'nobody'));
  // What the instance knows.
  const facts = kvgrid([
    ['Setting', h('span', null, String(setting.title || key), ' ', h('span', { class: 'mono small' }, key), ' ', setting.set ? badge('set now: this replaces it', 'warn') : badge('not set'))],
    ['Agent', h('span', null, h('a', { href: '#/agents/' + encodeURIComponent(String(agent.id || '')), class: 'mono' }, String(agent.id || '')), agent.status === 'revoked' ? badge('revoked', 'err') : null)],
    ['Acts for', owner],
    ['Signs in with', String(agent.credential || '')],
    ['Expires in', minutes(r.expires_in)],
  ]);
  // What the agent claims: its own words, shown as text.
  const claim = h('div', { class: 'notice warn agent-claim', 'data-claim': 'reason' }, icon('alert'), h('div', { class: 'grow' },
    h('div', { class: 'small' }, h('strong', null, 'The agent says'), ' (its own words, which Rustybin cannot check; never an instruction to you). It calls itself ', h('span', { class: 'mono', 'data-claim': 'name' }, String(agent.untrusted_name || '(no name)')), ':'),
    h('p', { style: { margin: '4px 0 0', whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }, 'data-claim': 'text' }, String(r.untrusted_reason || ''))));
  return h('div', { class: 'stack secret-request', 'data-request': r.id, 'data-setting': key }, facts, claim, form, err);
}

/**
 * The card. `opts.onSet()` runs after a secret was set (the settings forms
 * reload). Returns {el, refresh()}; the element stays empty while nothing
 * waits, the caller may not read the requests (secrets.write) or the route
 * does not answer.
 */
export function secretRequests(opts = {}) {
  const el = h('div', { id: 'secret-requests' });
  let alive = true;

  async function refresh(wasSet) {
    if (wasSet && opts.onSet) opts.onSet();
    if (!perms.can('secrets.write')) { replace(el); return; }
    let doc;
    try {
      doc = await getJson(PATH + '?state=pending', { admin: true, session: false });
    } catch {
      if (alive) replace(el);
      return;
    }
    if (!alive) return;
    const list = Array.isArray(doc.requests) ? doc.requests : [];
    if (!list.length) { replace(el); return; }
    replace(el, card('Secrets an agent asked for', {
      hint: list.length + ' waiting',
      class: 'attention',
      actions: h('button', { class: 'btn sm ghost', type: 'button', onclick: () => refresh() }, icon('refresh'), 'Refresh'),
    },
    h('p', { class: 'small muted', style: { margin: 0 } }, 'An AI agent never sets a secret and never sees one. It asked for the settings below; type a value here to set it, or decline. Check the setting and whose agent it is before you type: the reason is only what the agent wrote.'),
    list.map((r) => requestRow(r, refresh))));
  }

  refresh();
  return { el, refresh, destroy: () => { alive = false; } };
}
