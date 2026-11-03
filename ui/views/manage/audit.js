// Management, Audit: who did what, and when. GET /_rustybin/audit (audit.read)
// with the filters (actor, action, target, result, since), newest first,
// "Older entries" pages back with next_cursor; Live tail follows
// GET /_rustybin/audit/stream with send({stream: true}) and sseParser (never
// EventSource: the control token goes in a header). An entry links to the
// user (Users and roles), the mock, the agent or the saved tests it names.
// The retention settings (audit.*) are the settings form of the `audit`
// group. The same on every surface: rustybin audit list | tail, the
// list_audit tool, rustybin.yaml's settings (never the log itself).
import { h, replace, clear, card, field, input, select, notice, toast, icon, badge, spinner } from '../../lib/dom.js';
import { SERVER, send, sseParser, readText } from '../../lib/http.js';
import { settingsForm } from '../../lib/settingsform.js';
import * as perms from '../../lib/perms.js';

const BASE = '/_rustybin/audit';
const FEED_MAX = 500;

function when(iso) {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleString([], { hour12: false });
}

function actorView(a) {
  const kind = a.kind || 'anonymous';
  const label = kind === 'user'
    ? h('a', { href: '#/manage/users', title: 'User id ' + (a.id || '') }, a.name || a.id || 'user')
    : h('span', null, a.name ? `${kind}:${a.name}` : kind);
  return h('div', { class: 'stack-sm' }, label,
    kind !== 'user' ? null : h('span', { class: 'small muted mono' }, a.id || ''),
    a.agent ? h('a', { class: 'small', href: '#/agents/' + encodeURIComponent(a.agent), 'data-agent': a.agent }, 'agent ' + a.agent.slice(0, 8)) : null);
}

function targetLink(t) {
  const text = `${t.kind}:${t.name}`;
  const href = t.kind === 'mock' ? '#/mocks/' + encodeURIComponent(t.name)
    : t.kind === 'user' || t.kind === 'role' || t.kind === 'project' ? '#/manage/users'
      : t.kind === 'agent' ? '#/agents/' + encodeURIComponent(t.name)
        : t.kind === 'test' || t.kind === 'schedule' ? '#/tests'
          : t.kind === 'checkpoint' ? '#/manage/checkpoints'
            : t.kind === 'item' ? '#/manage/portal'
              : null;
  return href ? h('a', { class: 'mono small', href, 'data-target': text }, text) : h('span', { class: 'mono small', 'data-target': text }, text);
}

function resultView(r) {
  const kind = r.outcome === 'ok' ? 'ok' : r.outcome === 'refused' ? 'err' : r.outcome === 'asked' ? 'info' : 'warn';
  const label = r.status ? String(r.status) : r.outcome;
  return h('div', { class: 'stack-sm' }, badge(r.outcome === 'refused' ? 'refused' : label, kind),
    r.permission ? h('span', { class: 'small mono', title: 'The permission the caller lacked' }, r.permission)
      : r.reason ? h('span', { class: 'small muted mono' }, r.reason) : null);
}

function row(e) {
  const targets = (e.targets || []).map(targetLink);
  return h('tr', { 'data-audit': String(e.id), 'data-outcome': e.result.outcome },
    h('td', { class: 'small nowrap' }, when(e.time)),
    h('td', { class: 'small' }, actorView(e.actor)),
    h('td', { class: 'small' }, h('div', { class: 'stack-sm' },
      h('span', { class: 'mono' }, e.action),
      e.event ? badge(e.event, 'info') : null,
      e.via !== 'api' ? h('span', { class: 'small muted' }, 'via ' + e.via) : null)),
    h('td', null, targets.length ? h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } }, ...targets) : h('span', { class: 'muted small' }, '')),
    h('td', null, resultView(e.result)),
    h('td', { class: 'small muted mono' }, e.address || '', e.request_id ? h('div', { title: 'Request id' }, e.request_id.slice(0, 13)) : null));
}

function size(n) {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + ' GiB';
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + ' MiB';
  if (n >= 1 << 10) return (n / (1 << 10)).toFixed(1) + ' KiB';
  return n + ' B';
}

export function auditPanel(doc, reload) {
  const el = h('div', { class: 'stack' });
  let alive = true;
  let entries = [];
  let cursor = null;
  let feedCtrl = null;

  const actor = input({ id: 'audit-actor', placeholder: 'a user, admin_token, token, key, ...' });
  const action = input({ id: 'audit-action', placeholder: 'mocks, login, invite_user, ...' });
  const target = input({ id: 'audit-target', placeholder: 'mock:orders, user:alice, setting:egress.allow' });
  const result = select([['', 'Any result'], ['ok', 'ok'], ['error', 'error'], ['refused', 'refused'], ['asked', 'asked (waiting for a confirmation)']], '', { id: 'audit-result' });
  const since = select([['', 'Everything kept'], ['15m', 'The last 15 minutes'], ['1h', 'The last hour'], ['24h', 'The last 24 hours'], ['7d', 'The last 7 days']], '', { id: 'audit-since' });
  const statusEl = h('div', { class: 'row', id: 'audit-log-status', style: { gap: '6px', flexWrap: 'wrap' } });
  const tableEl = h('div', { id: 'audit-entries' }, spinner('Reading the audit log'));
  const moreEl = h('div');
  const liveBtn = h('button', { class: 'btn sm', type: 'button', id: 'audit-live', 'aria-pressed': 'false' }, icon('play'), 'Live tail');

  function query(withCursor) {
    const q = new URLSearchParams();
    for (const [k, c] of [['actor', actor], ['action', action], ['target', target], ['result', result], ['since', since]]) {
      if (c.value.trim()) q.set(k, c.value.trim());
    }
    if (withCursor && cursor) q.set('cursor', cursor);
    const s = q.toString();
    return s ? '?' + s : '';
  }

  function renderStatus(log) {
    clear(statusEl);
    if (!log) return;
    statusEl.appendChild(badge(`${log.held} of ${log.capacity} in memory`, 'info'));
    statusEl.appendChild(log.persisted ? badge(`${log.files} file(s), ${size(log.bytes || 0)} in <state>/audit`, 'ok') : badge('memory only (no state directory)', 'warn'));
    statusEl.appendChild(h('span', { class: 'small muted' }, `kept ${log.retention_days} day(s), at most ${size(log.max_bytes)}`));
    if (log.not_written) statusEl.appendChild(badge(`${log.not_written} not written to a file`, 'warn'));
    if (log.suppressed) statusEl.appendChild(badge(`${log.suppressed} unauthenticated entries over the per-minute cap`, 'warn'));
    if (log.write_error) statusEl.appendChild(badge('write error: ' + log.write_error, 'err'));
  }

  function renderTable() {
    if (!entries.length) {
      replace(tableEl, h('p', { class: 'muted', id: 'audit-empty' }, 'No entries match.'));
      return;
    }
    replace(tableEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'audit-table' },
      h('thead', null, h('tr', null, h('th', null, 'Time'), h('th', null, 'Who'), h('th', null, 'What'), h('th', null, 'Targets'), h('th', null, 'Result'), h('th', null, 'From'))),
      h('tbody', null, entries.map(row)))));
  }

  function renderMore() {
    replace(moreEl, cursor
      ? h('button', { class: 'btn sm', type: 'button', id: 'audit-older', onclick: () => load(true) }, 'Older entries')
      : null);
  }

  async function load(older) {
    if (!older) { cursor = null; entries = []; }
    const r = await send({ control: true, session: false, url: SERVER + BASE + query(older), timeout: 15000 });
    if (!alive) return;
    if (!r.ok) {
      const why = (r.json && (r.json.permission ? 'it needs the ' + r.json.permission + ' permission' : r.json.error)) || r.error || r.status;
      replace(tableEl, notice(r.status === 403 ? 'warn' : 'err', 'The audit log could not be read: ' + why));
      return;
    }
    entries = entries.concat(r.json.entries || []);
    cursor = r.json.next_cursor || null;
    renderStatus(r.json.log);
    renderTable();
    renderMore();
    // What the table shows (the conformance run waits for it).
    tableEl.dataset.query = query(false);
  }

  function renderLive() {
    const on = !!feedCtrl;
    liveBtn.setAttribute('aria-pressed', String(on));
    replace(liveBtn, icon(on ? 'pause' : 'play'), on ? 'Stop the live tail' : 'Live tail');
  }

  async function startFeed() {
    const ctrl = new AbortController();
    feedCtrl = ctrl;
    renderLive();
    // The filters of the form apply to the tail too (not the cursor).
    const r = await send({ control: true, session: false, url: SERVER + BASE + '/stream' + query(false), stream: true, timeout: 15000, signal: ctrl.signal });
    if (r.error || !r.response || !(r.contentType || '').includes('event-stream')) {
      if (feedCtrl === ctrl) { feedCtrl = null; renderLive(); }
      toast('The live tail is not available: ' + (r.error || (r.json && r.json.error) || r.status), 'err');
      return;
    }
    liveBtn.dataset.connected = 'true';
    try {
      await readText(r.response, sseParser((ev) => {
        if (ev.event !== 'entry' || !alive) return;
        let e = null;
        try { e = JSON.parse(ev.data); } catch { e = null; }
        if (!e) return;
        entries.unshift(e);
        entries.length = Math.min(entries.length, FEED_MAX);
        renderTable();
      }), ctrl.signal);
    } catch { /* the stream broke off */ }
    if (feedCtrl === ctrl) { feedCtrl = null; if (alive) renderLive(); }
  }

  function stopFeed() {
    delete liveBtn.dataset.connected;
    if (feedCtrl) feedCtrl.abort();
    feedCtrl = null;
    renderLive();
  }

  liveBtn.addEventListener('click', () => (feedCtrl ? stopFeed() : startFeed()));

  const form = h('form', { class: 'row', id: 'audit-filters', style: { gap: '8px', flexWrap: 'wrap', alignItems: 'flex-end' } },
    field('Who', actor), field('What', action), field('Target', target), field('Result', result), field('When', since),
    h('button', { class: 'btn primary', type: 'submit', id: 'audit-apply' }, icon('check'), 'Filter'));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const live = !!feedCtrl;
    if (live) stopFeed();
    load(false);
    if (live) startFeed();
  });

  el.appendChild(card('Audit log', { hint: 'GET ' + BASE + ' (audit.read)', actions: h('div', { class: 'row nowrap' }, liveBtn, h('button', { class: 'btn sm ghost', type: 'button', id: 'audit-refresh', onclick: () => load(false) }, icon('refresh'), 'Refresh')) },
    h('div', { class: 'stack' },
      h('p', { class: 'muted small', style: { margin: 0 } }, 'Every change to the instance (each mutating control route and management tool), every sign-in and failed sign-in, invitations and role changes, single sign-on with the groups that mapped, portal sign-ins and items opened or closed, agent revocations, config applies, git pulls with the commit, and every refusal that names a permission. Names only: never a value, a body, a header, a password or a token.'),
      form, statusEl, tableEl, moreEl)));

  if (doc) {
    const settingsCard = settingsForm(doc, 'audit', { saveLabel: 'Save audit settings', onSaved: (d) => { reload(d); load(false); } });
    el.appendChild(card('Retention', { hint: 'the audit.* settings (settings.security)' }, settingsCard.el));
  }

  if (!perms.can('audit.read')) {
    replace(tableEl, notice('warn', icon('lock'), 'Reading the audit log needs the audit.read permission (the admin role has it).'));
    liveBtn.disabled = true;
  } else {
    load(false);
  }
  // Leaving the panel ends the tail.
  const observer = new MutationObserver(() => {
    if (!el.isConnected) { alive = false; stopFeed(); observer.disconnect(); }
  });
  queueMicrotask(() => { if (el.parentNode) observer.observe(document.body, { childList: true, subtree: true }); });
  return el;
}
