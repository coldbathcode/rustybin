// AI agents: every client that talks to this instance through the
// management MCP server (/_rustybin/mcp), what it did and the operator's
// controls over it. The list (name and version, the user it acts for with
// an owner filter, credential, first and last seen, calls today, status;
// without agents.read the server answers the caller's own agents only, and
// a user's own agents keep their controls without agents.manage), a detail
// panel per agent (calls per tool with
// their outcomes, last errors, sessions, the rights form with a ceiling and
// tool allow or deny chips, a note, Revoke and Restore, End sessions), the
// live feed of calls, the default ceiling and the rules of rustybin.yaml,
// and the Connect an agent panel. AI: "Explain what this agent did".
import { h, replace, clear, card, kvgrid, notice, empty, icon, badge, toast, select, textarea, spinner, fmtTime } from '../lib/dom.js';
import { SERVER, send, getJson, sseParser, readText } from '../lib/http.js';
import { agentPanel } from '../lib/agent.js';
import { settingsForm } from '../lib/settingsform.js';
import * as assist from '../lib/assist.js';
import * as perms from '../lib/perms.js';

const BASE = '/_rustybin/agents';
const FEED_MAX = 50;

function when(iso) {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleString([], { hour12: false, month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

function statusBadgeOf(s) {
  return badge(s, s === 'revoked' ? 'err' : s === 'active' ? 'ok' : 'outline');
}

function ceilingBadge(r) {
  const from = r.source === 'agent' ? 'its own' : r.source === 'rule' ? 'rule ' + (r.rule + 1) : r.source === 'role' ? 'role default' : 'default';
  const label = r.ceiling_kind === 'role' ? 'role ' + r.ceiling : r.ceiling;
  return h('span', { class: 'row nowrap', style: { gap: '4px' } }, badge(label, r.ceiling === 'full' ? 'outline' : r.ceiling_known === false ? 'err' : 'warn'), h('span', { class: 'tiny muted' }, from));
}

/** The owner filter's key of an agent: its user's id, or '-' for a credential of its own. */
function ownerKey(a) {
  return a.owner && a.owner.user_id ? a.owner.user_id : '-';
}

/** Is the agent the signed-in user's own? */
function isMine(a, list) {
  return !!(list && list.you && a.owner && a.owner.user_id === list.you);
}

/** Guard a control with `p`, unless the agent is the caller's own (the server checks again). */
function guardUnlessMine(el, p, a, list) {
  return isMine(a, list) ? el : perms.guard(el, p);
}

function ownerCell(a, list) {
  const mine = isMine(a, list);
  return h('span', { class: 'row nowrap', style: { gap: '4px' } },
    h('span', { class: a.owner ? '' : 'muted' }, a.owner_label || ''),
    mine ? badge('you', 'ok') : null,
    a.owner && a.owner.status && a.owner.status !== 'active' ? badge(a.owner.status, 'err') : null);
}

async function call(method, path, body) {
  const opts = { control: true, url: SERVER + path, method, session: false };
  if (body !== undefined) { opts.headers = { 'Content-Type': 'application/json' }; opts.body = JSON.stringify(body); }
  const r = await send(opts);
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error((r.json && r.json.error) || r.status + ' ' + r.statusText);
  return r.json;
}

export default {
  id: 'agents',
  mount(root, { rest }) {
    let alive = true;
    let list = null;
    let listError = null;
    let selected = rest || null;
    let detail = null;
    let feedCtrl = null;
    let ownerFilter = '';
    const feed = [];

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'AI agents'),
        h('p', null, 'Every agent that manages this instance through its MCP server: what it called, what failed, and what it may do. Limit an agent below what its credential allows, deny it single tools, end its sessions or revoke it.')),
      h('button', { class: 'btn', type: 'button', onclick: () => reload() }, icon('refresh'), 'Refresh')));

    const listEl = h('div');
    const detailEl = h('div', { id: 'agent-detail' });
    const feedEl = h('div');
    const policyEl = h('div');
    root.appendChild(h('div', { class: 'stack' },
      listEl,
      detailEl,
      feedEl,
      policyEl,
      card('Connect an agent', { hint: 'claude mcp add, or the mcpServers JSON' }, h('div', { id: 'agent-connect' }, agentPanel()))));

    async function loadList() {
      try { list = await getJson(BASE, { session: false }); listError = null; } catch (e) { listError = e; }
    }

    async function loadDetail() {
      if (!selected) { detail = null; return; }
      try { detail = await getJson(BASE + '/' + encodeURIComponent(selected), { session: false }); } catch (e) { detail = { error: e.message }; }
    }

    async function reload() {
      await Promise.all([loadList(), loadDetail()]);
      if (!alive) return;
      renderList();
      renderDetail();
      renderPolicy();
    }

    // ── The list ──
    function renderList() {
      clear(listEl);
      if (listError) { listEl.appendChild(card('Agents', {}, notice('err', 'The agents could not be read: ' + listError.message))); return; }
      if (!list) { listEl.appendChild(card('Agents', {}, spinner('Loading the agents'))); return; }
      const all = list.agents || [];
      const open = list.control_auth === 'open';
      // The owner filter: everyone, mine, one user, or the credentials that act for nobody.
      const owners = new Map();
      for (const a of all) if (!owners.has(ownerKey(a))) owners.set(ownerKey(a), ownerKey(a) === '-' ? 'No user (admin token, JWTs, keys)' : a.owner_label);
      if (ownerFilter && ownerFilter !== 'mine' && !owners.has(ownerFilter)) ownerFilter = '';
      const agents = all.filter((a) => !ownerFilter || (ownerFilter === 'mine' ? isMine(a, list) : ownerKey(a) === ownerFilter));
      const filter = select([['', 'Everyone (' + all.length + ')'], ...(list.you ? [['mine', 'Mine']] : []), ...[...owners].map(([k, label]) => [k, label])], ownerFilter, { id: 'agents-owner-filter', 'aria-label': 'Owner' });
      filter.addEventListener('change', () => { ownerFilter = filter.value; renderList(); });
      const rows = agents.map((a) => h('tr', { 'data-agent': a.id, 'data-owner': ownerKey(a), class: a.id === selected ? 'hl' : '', style: { cursor: 'pointer' }, onclick: () => choose(a.id) },
        h('td', null, h('button', { class: 'linkish', type: 'button', 'aria-label': 'Open the agent ' + a.name, onclick: (e) => { e.stopPropagation(); choose(a.id); } }, h('strong', null, a.name)),
          a.version ? h('span', { class: 'tiny muted' }, ' ' + a.version) : null,
          h('div', { class: 'tiny muted' }, a.transport || '')),
        h('td', { class: 'small', 'data-col': 'owner' }, ownerCell(a, list)),
        h('td', { class: 'small' }, a.credential.label),
        h('td', { class: 'small nowrap' }, when(a.first_seen)),
        h('td', { class: 'small nowrap' }, when(a.last_seen)),
        h('td', { class: 'mono small' }, String(a.calls.today), h('span', { class: 'tiny muted' }, ' / ' + a.calls.total)),
        h('td', null, ceilingBadge(a.rights)),
        h('td', null, statusBadgeOf(a.status))));
      const table = h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'agents-table' },
        h('thead', null, h('tr', null, h('th', null, 'Agent'), h('th', null, 'Owner'), h('th', null, 'Credential'), h('th', null, 'First seen'), h('th', null, 'Last seen'), h('th', null, 'Calls today / all'), h('th', null, 'Ceiling'), h('th', null, 'Status'))),
        h('tbody', null, rows)));
      const ownOnly = !!list.owner && !perms.can('agents.read');
      listEl.appendChild(card('Agents', { hint: `${all.length} of at most ${list.capacity}` + (list.persisted ? '' : ', kept until a restart'), actions: all.length ? h('label', { class: 'row nowrap small', style: { gap: '6px' } }, 'Owner', filter) : null },
        open ? notice('warn', icon('alert'), h('span', null, 'The control plane is open: anyone may connect under any client name, so rights and revocations here are advisory. Turn on the console sign-in (RUSTYBIN_CONTROL_AUTH=password) to control agents for real.')) : null,
        ownOnly ? notice('info', 'Your own agents: they act for you and never do more than you may. You may limit, revoke and end the sessions of them; other people\'s agents need the agents.read permission.') : null,
        all.length ? (agents.length ? table : empty('No agent of this owner', 'Choose another owner above.', 'filter')) : empty('No agent yet', 'Agents appear here after their first call to /_rustybin/mcp. Connect one with the command below.', 'terminal')));
    }

    function choose(id) {
      location.hash = '#/agents/' + encodeURIComponent(id);
    }

    // ── One agent ──
    function renderDetail() {
      clear(detailEl);
      if (!selected) return;
      if (!detail) { detailEl.appendChild(card('Agent', {}, spinner('Loading the agent'))); return; }
      if (detail.error) { detailEl.appendChild(card('Agent', {}, notice('err', 'The agent could not be read: ' + detail.error))); return; }
      const a = detail;
      const revoked = a.status === 'revoked';
      const close = h('button', { class: 'btn sm', type: 'button', 'aria-label': 'Close the agent', onclick: () => { location.hash = '#/agents'; } }, icon('x'));
      const actions = h('div', { class: 'row nowrap' },
        assist.assistButton('Explain what this agent did', 'explain', () => ({ kind: 'agent', subject: activityOf(a) })),
        close);
      detailEl.appendChild(card(a.name + (a.version ? ' ' + a.version : ''), { hint: a.credential.label, actions },
        h('div', { class: 'stack' },
          revoked ? notice('err', icon('lock'), h('span', null, `Revoked ${when(a.revoked.at)} by ${a.revoked.by}: every call is refused.`)) : null,
          kvgrid([
            ['Status', statusBadgeOf(a.status)],
            ['Acts for', ownerCell(a, list)],
            ['Credential', a.credential.label],
            ['Transport', a.transport || 'none yet'],
            ['First seen', when(a.first_seen)],
            ['Last seen', when(a.last_seen)],
            ['Calls', `${a.calls.total} (${a.calls.ok} ok, ${a.calls.error} errors, ${a.calls.denied} denied, ${a.calls.asked} asked to confirm); ${a.calls.today} today; tools listed ${a.calls.lists} times`],
            ['Sessions', `${a.sessions_open} open`],
            ['Tools allowed', `${a.tools_allowed} of ${a.available_tools.length}`],
          ]),
          controlsRow(a),
          rightsForm(a),
          toolsTable(a),
          errorsList(a))));
    }

    function activityOf(a) {
      return { name: a.name, version: a.version, credential: a.credential.kind, transport: a.transport, status: a.status, calls: a.calls, tools: a.tools, last_errors: a.last_errors, rights: a.rights, note: a.note };
    }

    function controlsRow(a) {
      const revoked = a.status === 'revoked';
      const revoke = guardUnlessMine(h('button', { class: 'btn' + (revoked ? '' : ' danger'), type: 'button', id: revoked ? 'agent-restore' : 'agent-revoke' }, icon(revoked ? 'unlock' : 'lock'), revoked ? 'Restore' : 'Revoke'), 'agents.manage', a, list);
      revoke.addEventListener('click', async () => {
        if (!revoked) {
          const warn = a.credential.via === 'sign-in'
            ? ' Its console sign-in tokens are revoked too: if it uses your own token, this console signs out.'
            : a.credential.kind === 'admin' ? ' The admin token itself cannot be revoked per agent (rotate it); this agent\'s calls are refused.' : '';
          if (!window.confirm(`Revoke ${a.name}? Its calls are refused and its sessions end.${warn}`)) return;
        }
        revoke.disabled = true;
        try {
          const r = await call('POST', `${BASE}/${encodeURIComponent(a.id)}/${revoked ? 'restore' : 'revoke'}`);
          toast(revoked ? 'Restored ' + a.name : `Revoked ${a.name}: ${r.sessions_ended} session(s) ended, ${r.tokens_revoked} token(s) revoked`);
          if (r.note) toast(r.note, 'warn');
          reload();
        } catch (e) { toast('Not changed: ' + e.message, 'err'); revoke.disabled = false; }
      });
      const end = guardUnlessMine(h('button', { class: 'btn', type: 'button', id: 'agent-end-sessions', disabled: !a.sessions_open }, icon('stop'), 'End sessions'), 'agents.manage', a, list);
      end.addEventListener('click', async () => {
        end.disabled = true;
        try { const r = await call('DELETE', `${BASE}/${encodeURIComponent(a.id)}/sessions`); toast(`${r.sessions_ended} session(s) ended`); reload(); }
        catch (e) { toast('Not ended: ' + e.message, 'err'); end.disabled = false; }
      });
      const sessions = (a.sessions || []).filter((s) => s.open);
      return h('div', { class: 'stack-sm' },
        h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } }, revoke, end),
        sessions.length ? h('p', { class: 'tiny muted', style: { margin: 0 } }, 'Open sessions: ', sessions.map((s) => s.id + '... since ' + when(s.opened)).join(', ')) : null);
    }

    function rightsForm(a) {
      const o = a.override || {};
      const r = a.rights;
      const inherited = r.source === 'agent' && o.ceiling ? null : r.ceiling;
      const roles = (list.roles || []).filter((n) => !list.ceilings.includes(n));
      if (o.ceiling && !list.ceilings.includes(o.ceiling) && !roles.includes(o.ceiling)) roles.push(o.ceiling);
      const NEAREST = { 'read-only': 'viewer', traffic: 'viewer + traffic.read', manage: 'editor', full: 'its user\'s own' };
      const ceil = select([['', 'From the rules (' + (inherited || 'see below') + ')'], ...list.ceilings.map((c) => [c, c + ' (' + NEAREST[c] + ')']), ...roles.map((n) => [n, 'role ' + n])], o.ceiling || '', { id: 'agent-ceiling', 'aria-label': 'Ceiling' });
      const marks = new Map();
      for (const t of o.allow || []) marks.set(t, 'allow');
      for (const t of o.deny || []) marks.set(t, 'deny');
      const chips = h('div', { class: 'pill-list', id: 'agent-tools' });
      const drawChips = () => {
        clear(chips);
        for (const t of a.available_tools) {
          const m = marks.get(t.name) || '';
          const label = t.name + (m === 'allow' ? ' (allowed)' : m === 'deny' ? ' (denied)' : '');
          const b = h('button', { class: 'btn sm' + (m === 'deny' ? ' danger' : m === 'allow' ? ' primary' : ''), type: 'button', 'data-tool': t.name, 'aria-label': t.name + ': ' + (m || 'from the rules'), title: t.title + ' (' + t.permission + (t.read_only ? ', read-only' : '') + '): click to allow, deny or reset', style: { opacity: !m && !t.allowed ? 0.55 : 1 } }, label);
          b.addEventListener('click', () => {
            marks.set(t.name, m === '' ? 'allow' : m === 'allow' ? 'deny' : '');
            drawChips();
          });
          chips.appendChild(b);
        }
      };
      drawChips();
      const note = textarea({ rows: 2, id: 'agent-note', 'aria-label': 'Note', class: 'textarea', maxlength: 500 });
      note.value = a.note || '';
      const out = h('div');
      const save = guardUnlessMine(h('button', { class: 'btn primary', type: 'submit', id: 'agent-save-rights' }, 'Save rights'), 'agents.manage', a, list);
      const reset = guardUnlessMine(h('button', { class: 'btn', type: 'button', id: 'agent-reset-rights' }, 'Back to the rules'), 'agents.manage', a, list);
      const form = h('form', { class: 'stack-sm', 'aria-label': 'Rights of ' + a.name },
        h('h3', { style: { margin: 0 } }, 'Rights'),
        h('p', { class: 'small muted', style: { margin: 0 } }, 'A ceiling caps the tools below what the credential allows (an agent that acts for a user never does more than that user): a role (the tools whose permission it grants), or read-only (read tools, no captured traffic), traffic (every read tool), manage (reads and mock changes), full. An allowed tool narrows the agent to the allowed ones; a denied tool is never listed nor callable.', isMine(a, list) && !perms.can('agents.manage') ? ' This agent is yours: you may limit it, not widen it again.' : ''),
        h('label', { class: 'field' }, h('span', { class: 'label' }, 'Ceiling'), ceil),
        h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Tools'), chips),
        h('label', { class: 'field' }, h('span', { class: 'label' }, 'Note'), note),
        h('div', { class: 'row', style: { gap: '8px' } }, save, reset),
        out);
      form.addEventListener('submit', async (e) => {
        e.preventDefault();
        const allow = [...marks].filter(([, m]) => m === 'allow').map(([t]) => t);
        const deny = [...marks].filter(([, m]) => m === 'deny').map(([t]) => t);
        const rights = {};
        if (ceil.value) rights.ceiling = ceil.value;
        if (allow.length) rights.allow = allow;
        if (deny.length) rights.deny = deny;
        try {
          await call('PATCH', `${BASE}/${encodeURIComponent(a.id)}`, { rights: Object.keys(rights).length ? rights : null, note: note.value.trim() || null });
          toast('Rights saved for ' + a.name);
          reload();
        } catch (err) { replace(out, notice('err', err.message)); }
      });
      reset.addEventListener('click', async () => {
        try { await call('PATCH', `${BASE}/${encodeURIComponent(a.id)}`, { rights: null }); toast('Back to the rules'); reload(); }
        catch (err) { replace(out, notice('err', err.message)); }
      });
      return form;
    }

    function toolsTable(a) {
      if (!a.tools.length) return h('p', { class: 'small muted', style: { margin: 0 } }, 'No tool called yet.');
      return h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'agent-tool-calls' },
        h('thead', null, h('tr', null, h('th', null, 'Tool'), h('th', null, 'Ok'), h('th', null, 'Errors'), h('th', null, 'Denied'), h('th', null, 'Asked'))),
        h('tbody', null, a.tools.map((t) => h('tr', null,
          h('td', { class: 'mono small' }, t.name),
          h('td', { class: 'mono small' }, String(t.ok)),
          h('td', { class: 'mono small' + (t.error ? ' err' : '') }, String(t.error)),
          h('td', { class: 'mono small' + (t.denied ? ' warn' : '') }, String(t.denied)),
          h('td', { class: 'mono small' }, String(t.asked)))))));
    }

    function errorsList(a) {
      if (!a.last_errors.length) return null;
      return h('div', { class: 'stack-sm' }, h('h3', { style: { margin: 0 } }, 'Last errors'),
        h('ul', { class: 'small stack-sm', style: { margin: 0, paddingLeft: '18px' } },
          a.last_errors.map((e) => h('li', null, h('span', { class: 'mono' }, e.tool), ' ', h('span', { class: 'tiny muted' }, when(e.at)), ': ', e.message))));
    }

    // ── The live feed ──
    function renderFeed() {
      clear(feedEl);
      const on = !!feedCtrl;
      const toggle = h('button', { class: 'btn sm', type: 'button', id: 'agents-live', 'aria-pressed': String(on) }, icon(on ? 'pause' : 'play'), on ? 'Stop the live feed' : 'Live feed');
      toggle.addEventListener('click', () => (feedCtrl ? stopFeed() : startFeed()));
      const lines = feed.length
        ? h('ul', { class: 'small mono stack-sm', id: 'agents-feed', style: { margin: 0, paddingLeft: '18px' } }, feed.map((f) => h('li', null, fmtTime(f.at), ' ', f.event ? h('strong', null, f.agent + ' ' + f.event) : [h('strong', null, f.name), ' ', f.tool, ' ', badge(f.outcome, f.outcome === 'ok' ? 'ok' : f.outcome === 'error' ? 'err' : 'warn')])))
        : h('p', { class: 'small muted', style: { margin: 0 } }, on ? 'Waiting for calls.' : 'Turn the feed on to watch the calls as they happen.');
      feedEl.appendChild(card('Live calls', { hint: 'tool calls as they happen', actions: toggle }, lines));
    }

    async function startFeed() {
      const ctrl = new AbortController();
      feedCtrl = ctrl;
      renderFeed();
      const r = await send({ control: true, url: SERVER + BASE + '/events', stream: true, timeout: 15000, signal: ctrl.signal, session: false });
      if (r.error || !r.response || !(r.contentType || '').includes('event-stream')) {
        if (feedCtrl === ctrl) { feedCtrl = null; renderFeed(); }
        toast('The live feed is not available: ' + (r.error || (r.json && r.json.error) || r.status), 'err');
        return;
      }
      try {
        await readText(r.response, sseParser((ev) => {
          let data = null;
          try { data = JSON.parse(ev.data); } catch { data = null; }
          if (!data || !alive) return;
          feed.unshift(data);
          feed.length = Math.min(feed.length, FEED_MAX);
          renderFeed();
        }), ctrl.signal);
      } catch { /* the stream broke off */ }
      if (feedCtrl === ctrl) { feedCtrl = null; if (alive) renderFeed(); }
    }

    function stopFeed() {
      if (feedCtrl) feedCtrl.abort();
      feedCtrl = null;
      renderFeed();
      reload();
    }

    // ── Defaults and rules ──
    async function renderPolicy() {
      if (!list) return;
      let doc = null;
      try { doc = await getJson('/_rustybin/settings', { session: false }); } catch { doc = null; }
      if (!alive) return;
      const p = list.policy;
      const src = { stored: 'saved in this instance', env: 'from RUSTYBIN_AGENTS_DEFAULT_CEILING', file: 'from rustybin.yaml', default: 'the default' }[p.default_source] || p.default_source;
      const rules = p.rules.length
        ? h('ol', { class: 'small stack-sm', id: 'agents-rules', style: { margin: 0, paddingLeft: '18px' } }, p.rules.map((r) => h('li', null,
          'name ', h('code', null, r.match.name || '*'), ', credential ', h('code', null, r.match.credential || 'any'),
          r.match.user ? [', user ', h('code', null, r.match.user)] : null,
          r.match.role ? [', role ', h('code', null, r.match.role)] : null, ': ',
          r.ceiling ? badge(r.ceiling, 'warn') : 'the default ceiling',
          r.allow.length ? [' allow ', h('span', { class: 'mono' }, r.allow.join(', '))] : null,
          r.deny.length ? [' deny ', h('span', { class: 'mono' }, r.deny.join(', '))] : null,
          r.note ? h('span', { class: 'muted' }, ' (' + r.note + ')') : null)))
        : h('p', { class: 'small muted', style: { margin: 0 } }, 'No rule: put an agents: section in rustybin.yaml (default_ceiling and rules with match, ceiling, allow, deny, note).');
      replace(policyEl, card('Defaults and rules', { hint: 'what applies before an agent has rights of its own' },
        h('div', { class: 'stack' },
          h('p', { class: 'small', style: { margin: 0 } }, 'The default ceiling is ', badge(p.default_ceiling, p.default_ceiling === 'full' ? 'outline' : 'warn'), ', ' + src + '. The default for a role of the agent\'s user comes before it, rules of rustybin.yaml before that (the first that matches); an agent\'s own rights win field by field. An agent that acts for a user never does more than the user.'),
          (p.default_ceiling_for || []).length ? h('p', { class: 'small', id: 'agents-role-defaults', style: { margin: 0 } }, 'Per role: ', p.default_ceiling_for.map((e, i) => [i ? ', ' : '', h('code', null, e.role), ' ', badge(e.ceiling, 'warn')])) : null,
          doc ? settingsForm(doc, 'agents', { saveLabel: 'Save the default ceiling', onSaved: () => reload() }).el : null,
          h('h3', { style: { margin: 0 } }, 'Rules of rustybin.yaml'),
          rules)));
    }

    function route(r) {
      selected = r || null;
      detail = null;
      renderDetail();
      if (list) renderList();
      loadDetail().then(() => { if (alive) renderDetail(); });
    }

    renderList();
    renderFeed();
    reload();

    const cleanup = () => { alive = false; if (feedCtrl) feedCtrl.abort(); };
    cleanup.onRoute = route;
    cleanup.destroy = cleanup;
    cleanup.assistContext = () => ({ agents: list && list.agents.map((a) => ({ name: a.name, credential: a.credential.kind, status: a.status, ceiling: a.rights.ceiling, calls: a.calls.total })), selected: detail && !detail.error ? activityOf(detail) : null });
    return cleanup;
  },
};
