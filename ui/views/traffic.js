// Live traffic inspector: every request Rustybin received (method, path,
// headers, body), live over SSE, with filters, gateway header highlighting,
// copy as curl and a two-request compare mode.
import { h, replace, clear, icon, tabs, notice, empty, methodBadge, statusText, statusBadge, fmtMs, fmtTime, fmtBytes, copyButton, kvgrid, input, select, segmented, checkbox, toast, debounce } from '../lib/dom.js';
import { bodyView, headersTable, diffView, normaliseBody, isGatewayHeader, codeBlock, jsonView } from '../lib/format.js';
import { SERVER, send, toCurl } from '../lib/http.js';
import * as traffic from '../lib/traffic.js';
import * as settings from '../lib/settings.js';
import * as auth from '../lib/auth.js';

const LIST_LIMIT = 400;
const SKIP_CURL_HEADERS = new Set(['host', 'content-length', 'connection', 'accept-encoding', 'x-request-id']);

function headerOf(entry, name) {
  const n = name.toLowerCase();
  const hit = (entry.headers || []).find((x) => x.name.toLowerCase() === n);
  return hit ? hit.value : '';
}

export function curlFor(entry, base) {
  const headers = (entry.headers || []).filter((x) => !SKIP_CURL_HEADERS.has(x.name.toLowerCase())).map((x) => [x.name, x.value]);
  return toCurl({ method: entry.method, url: (base || SERVER) + entry.uri.replace(/^https?:\/\/[^/]+/, ''), headers, body: entry.body || undefined });
}

function bodyOf(entry) {
  if (entry.body) return entry.body;
  if (entry.body_base64) return '';
  return '';
}

const state = settings.recall('traffic.state', {
  path: '', method: '', status: 'all', text: '', paused: false, selected: null, compare: false, a: null, b: null, pausedAt: null,
});

export default {
  id: 'traffic',
  mount(root) {
    settings.remember('traffic.state', state);
    let frozen = state.paused ? traffic.all().slice() : null;
    let dirty = true;
    let raf = null;
    const listEl = h('div', { class: 'list', role: 'listbox', 'aria-label': 'Captured requests' });
    const listInfo = h('div', { class: 'muted small' });
    const detailEl = h('div');
    const banner = h('div');
    const statusEl = h('div');

    // ── Toolbar ──
    const pathIn = input({ placeholder: '/path prefix', value: state.path, class: 'input sm mono', 'aria-label': 'Path prefix filter' });
    const textIn = input({ placeholder: 'Search headers, body, path', value: state.text, class: 'input sm', type: 'search', 'aria-label': 'Text search' });
    const methodSel = select([['', 'Any method'], 'GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS', 'HEAD'], state.method, { class: 'select sm', 'aria-label': 'Method filter' });
    const statusSeg = segmented([['all', 'All'], ['2', '2xx'], ['3', '3xx'], ['4', '4xx'], ['5', '5xx']], state.status, (v) => { state.status = v; schedule(); }, 'Status class filter');
    const onFilter = debounce(() => { state.path = pathIn.value.trim(); state.text = textIn.value.trim().toLowerCase(); schedule(); }, 120);
    pathIn.addEventListener('input', onFilter);
    textIn.addEventListener('input', onFilter);
    methodSel.addEventListener('change', () => { state.method = methodSel.value; schedule(); });

    const pauseBtn = h('button', { class: 'btn', type: 'button' });
    const compareBtn = h('button', { class: 'btn', type: 'button', title: 'Select two requests to diff their headers and bodies' }, icon('diff'), 'Compare');
    const clearBtn = h('button', { class: 'btn danger', type: 'button' }, icon('trash'), 'Clear');
    const sessionOnly = checkbox('Only this console session', settings.get('trafficSessionOnly'), (v) => settings.set('trafficSessionOnly', v));

    function renderPause() {
      replace(pauseBtn, icon(state.paused ? 'play' : 'pause'), state.paused ? 'Resume' : 'Pause');
      pauseBtn.classList.toggle('active', state.paused);
      compareBtn.classList.toggle('active', state.compare);
    }
    pauseBtn.addEventListener('click', () => {
      state.paused = !state.paused;
      frozen = state.paused ? traffic.all().slice() : null;
      renderPause();
      schedule();
    });
    compareBtn.addEventListener('click', () => {
      state.compare = !state.compare;
      if (state.compare) { state.a = state.selected; state.b = null; }
      renderPause();
      schedule();
      renderDetail();
    });
    clearBtn.addEventListener('click', clearAll);
    renderPause();

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Live traffic'),
        h('p', null, 'Every request that reached Rustybin, as it arrived: what the gateway actually sent upstream. Headers that gateways typically add or rewrite are highlighted.')),
      h('div', { class: 'row' }, pauseBtn, compareBtn, clearBtn)));

    root.appendChild(h('div', { class: 'stack' }, statusEl, banner,
      h('div', { class: 'card' },
        h('div', { class: 'card-head' },
          h('div', { class: 'filters' },
            h('div', { style: { width: '170px' } }, pathIn), methodSel, statusSeg.el,
            h('div', { style: { flex: '1', minWidth: '180px' } }, textIn)),
          settings.instance.publicMode ? null : sessionOnly.el),
        h('div', { class: 'split' },
          h('div', { class: 'pane' }, h('div', { style: { padding: '6px 12px', borderBottom: '1px solid var(--border)' } }, listInfo), h('div', { class: 'pane-scroll' }, listEl)),
          h('div', { class: 'pane' }, h('div', { class: 'pane-scroll' }, detailEl))))));

    function matches(e) {
      if (state.path && !e.path.startsWith(state.path)) return false;
      if (state.method && e.method !== state.method) return false;
      if (state.status !== 'all' && String(Math.floor(e.status / 100)) !== state.status) return false;
      if (state.text) {
        const hay = (e.uri + ' ' + (e.body || '') + ' ' + (e.headers || []).map((x) => x.name + ': ' + x.value).join(' ')).toLowerCase();
        if (!hay.includes(state.text)) return false;
      }
      return true;
    }

    function source() { return frozen || traffic.all(); }

    function schedule() {
      dirty = true;
      if (raf) return;
      raf = setTimeout(() => { raf = null; if (dirty) renderList(); }, 120);
    }

    let lastTopId = null;
    function renderList() {
      dirty = false;
      const all = source();
      const filtered = all.filter(matches);
      const shown = filtered.slice(0, LIST_LIMIT);
      clear(listEl);
      if (!shown.length) {
        const ts = traffic.getStatus();
        listEl.appendChild(all.length
          ? empty('No request matches the filters', 'Clear the filters to see everything.', 'traffic')
          : empty('Waiting for requests', ts.status === 'needs-session'
            ? 'Public mode: set a session in the console settings and send requests with the X-Rustybin-Session header.'
            : `Send any request to ${SERVER} (directly or through your gateway) and it shows up here instantly.`, 'inbox'));
      }
      for (const e of shown) {
        const selected = state.compare ? (e.id === state.a || e.id === state.b) : e.id === state.selected;
        const row = h('button', {
          class: 'list-item traffic-row' + (state.compare && e.id === state.a ? ' compare-a' : '') + (state.compare && e.id === state.b ? ' compare-b' : '') + (e.id !== lastTopId && lastTopId && !state.paused && shown.indexOf(e) < 3 ? ' fresh' : ''),
          type: 'button', role: 'option', 'aria-selected': String(selected),
          onclick: () => pick(e.id),
        },
        h('span', { class: 'mono tiny muted col-time' }, fmtTime(e.timestamp).slice(0, 8)),
        methodBadge(e.method),
        h('span', { class: 'mono ellipsis', title: e.uri }, e.path + (e.query ? '?' + e.query : '')),
        statusText(e.status),
        h('span', { class: 'mono tiny muted col-lat', style: { textAlign: 'right' } }, fmtMs(e.latency_ms)));
        if (state.compare && (e.id === state.a || e.id === state.b)) {
          row.appendChild(h('span', { class: 'sr-only' }, e.id === state.a ? 'compare A' : 'compare B'));
        }
        listEl.appendChild(row);
      }
      lastTopId = shown.length ? shown[0].id : null;
      listInfo.textContent = `${filtered.length} of ${all.length} requests` + (filtered.length > LIST_LIMIT ? ` (showing newest ${LIST_LIMIT})` : '') + (state.paused ? ' | paused' : '');
      renderBanner();
    }

    function renderBanner() {
      if (state.paused && frozen) {
        const n = traffic.all().length - frozen.length;
        replace(banner, n > 0 ? notice('warn', `${n} new request${n === 1 ? '' : 's'} arrived while paused. `,
          h('button', { class: 'btn sm', type: 'button', onclick: () => pauseBtn.click() }, 'Resume')) : notice('', 'Paused: the list is frozen until you resume.'));
      } else {
        replace(banner);
      }
    }

    function renderStatus() {
      const ts = traffic.getStatus();
      if (ts.status === 'error' || ts.status === 'needs-session') replace(statusEl, notice(ts.status === 'needs-session' ? 'warn' : 'err', ts.error));
      else replace(statusEl);
    }

    function pick(id) {
      if (state.compare) {
        if (state.a === id) state.a = null;
        else if (state.b === id) state.b = null;
        else if (!state.a) state.a = id;
        else if (!state.b) state.b = id;
        else { state.a = state.b; state.b = id; }
      } else {
        state.selected = id;
      }
      schedule();
      renderDetail();
    }

    function renderDetail() {
      if (state.compare) return renderCompare();
      const e = state.selected && traffic.find(state.selected);
      if (!e) {
        replace(detailEl, empty('Select a request', 'Pick a request on the left to see its headers, body and query. Use Compare to diff two requests (for example a direct call and the same call through the gateway).', 'traffic'));
        return;
      }
      replace(detailEl, detail(e));
    }

    function detail(e) {
      const ct = headerOf(e, 'content-type');
      const gwCount = (e.headers || []).filter((x) => isGatewayHeader(x.name)).length;
      const params = e.query ? Array.from(new URLSearchParams(e.query)) : [];
      const t = tabs([
        { id: 'headers', label: `Headers (${(e.headers || []).length})`, render: () => h('div', null,
          gwCount ? h('div', { class: 'small muted', style: { padding: '8px 12px' } }, `${gwCount} header${gwCount === 1 ? '' : 's'} commonly added or rewritten by gateways and proxies`) : null,
          headersTable((e.headers || []).map((x) => [x.name, x.value]))) },
        { id: 'body', label: 'Body' + (e.body_size ? ` (${fmtBytes(e.body_size)})` : ''), render: () => h('div', { style: { padding: '12px' } },
          e.body_base64 ? h('div', { class: 'stack-sm' }, notice('', `Binary body (${fmtBytes(e.body_size)}), shown as base64.`), codeBlock(e.body_base64)) : bodyView(bodyOf(e), ct, { cls: 'tall' }),
          e.body_truncated ? notice('warn', 'The body was truncated to 64 KB in the inspector.') : null) },
        { id: 'query', label: `Query (${params.length})`, render: () => headersTable(params, { noHighlight: true, keyLabel: 'Parameter', emptyText: 'No query string' }) },
        { id: 'raw', label: 'Raw', render: () => h('div', { style: { padding: '12px' } }, jsonView(e, 'tall')) },
      ], settings.recall('traffic.tab', 'headers'), (id) => settings.remember('traffic.tab', id));
      return h('div', null,
        h('div', { style: { padding: '12px 14px', borderBottom: '1px solid var(--border)' }, class: 'stack-sm' },
          h('div', { class: 'row' }, methodBadge(e.method), h('span', { class: 'mono break grow', style: { fontWeight: 600 } }, e.uri), statusBadge(e.status)),
          kvgrid([
            ['Received', new Date(e.timestamp).toLocaleString()],
            ['Latency', fmtMs(e.latency_ms) + ' (time to response headers)'],
            ['Client IP', e.client_ip],
            ['Request id', e.request_id],
            ['Session', e.session || undefined],
            ['HTTP version', e.version],
          ]),
          h('div', { class: 'row' },
            copyButton(() => curlFor(e), { label: 'Copy as curl', toast: 'curl command copied (replays directly to Rustybin)' }),
            jsonLink(e),
            h('button', { class: 'btn sm', type: 'button', onclick: () => { state.compare = true; state.a = e.id; state.b = null; renderPause(); schedule(); renderDetail(); } }, icon('diff'), 'Compare with...'))),
        t.el);
    }

    // Raw JSON of an entry. With control-plane auth a plain link would lack
    // the token, so fetch it (with the Authorization header) into a blob.
    function jsonLink(e) {
      const url = SERVER + '/_rustybin/requests/' + encodeURIComponent(e.id) + (e.session ? '?session=' + encodeURIComponent(e.session) : '');
      if (!auth.required()) return h('a', { class: 'btn sm', href: url, target: '_blank', rel: 'noopener' }, icon('external'), 'JSON');
      return h('button', { class: 'btn sm', type: 'button', onclick: async () => {
        const r = await send({ url, session: false });
        if (!r.ok) { toast(r.error || ('JSON not available: ' + r.status), 'err'); return; }
        const blob = new Blob([JSON.stringify(r.json, null, 2)], { type: 'application/json' });
        const href = URL.createObjectURL(blob);
        window.open(href, '_blank', 'noopener');
        setTimeout(() => URL.revokeObjectURL(href), 60000);
      } }, icon('external'), 'JSON');
    }

    function renderCompare() {
      const a = state.a && traffic.find(state.a);
      const b = state.b && traffic.find(state.b);
      if (!a || !b) {
        replace(detailEl, h('div', { style: { padding: '14px' }, class: 'stack' },
          notice('', h('strong', null, 'Compare mode. '), `Select two requests in the list: ${a ? 'A is selected, now pick B' : 'pick A first'}. Typical demo: call an endpoint directly, then through the gateway, and compare what the upstream received.`),
          h('div', { class: 'row' }, h('button', { class: 'btn sm', type: 'button', onclick: () => compareBtn.click() }, 'Exit compare mode'))));
        return;
      }
      replace(detailEl, compareView(a, b));
    }

    function compareView(a, b) {
      const mapOf = (e) => {
        const m = new Map();
        for (const x of e.headers || []) {
          const k = x.name.toLowerCase();
          m.set(k, m.has(k) ? m.get(k) + ', ' + x.value : x.value);
        }
        return m;
      };
      const ma = mapOf(a), mb = mapOf(b);
      const names = Array.from(new Set([...ma.keys(), ...mb.keys()])).sort();
      let hideSame = settings.recall('traffic.hideSame', true);
      const tableHost = h('div');
      function renderTable() {
        const tbody = h('tbody');
        let changes = 0;
        for (const n of names) {
          const va = ma.get(n), vb = mb.get(n);
          const kind = va === undefined ? 'add' : vb === undefined ? 'del' : va === vb ? 'same' : 'chg';
          if (kind !== 'same') changes++;
          if (hideSame && kind === 'same') continue;
          tbody.appendChild(h('tr', { class: kind === 'same' ? null : 'diff-' + kind },
            h('td', { class: 'k' }, n, isGatewayHeader(n) ? h('span', { class: 'tag-gw' }, 'gateway') : null),
            h('td', { class: 'v' }, va === undefined ? h('span', { class: 'muted' }, '(absent)') : va),
            h('td', { class: 'v' }, vb === undefined ? h('span', { class: 'muted' }, '(absent)') : vb)));
        }
        if (!tbody.children.length) tbody.appendChild(h('tr', null, h('td', { colspan: '3', class: 'muted' }, 'Headers are identical.')));
        replace(tableHost, h('div', { class: 'small muted', style: { padding: '8px 12px' } }, `${changes} header difference${changes === 1 ? '' : 's'}: green only in B, red only in A, yellow changed.`),
          h('table', { class: 'table' }, h('thead', null, h('tr', null, h('th', null, 'Header'), h('th', null, 'A'), h('th', null, 'B'))), tbody));
      }
      renderTable();
      const hide = checkbox('Hide identical headers', hideSame, (v) => { hideSame = v; settings.remember('traffic.hideSame', v); renderTable(); });
      const line = (e) => `${e.method} ${e.uri}\nstatus ${e.status}, ${fmtMs(e.latency_ms)}, client ${e.client_ip || '?'}`;
      return h('div', null,
        h('div', { class: 'stack-sm', style: { padding: '12px 14px', borderBottom: '1px solid var(--border)' } },
          h('div', { class: 'row' }, h('span', { class: 'badge info' }, 'A'), methodBadge(a.method), h('span', { class: 'mono ellipsis grow' }, a.uri), h('span', { class: 'tiny muted' }, fmtTime(a.timestamp))),
          h('div', { class: 'row' }, h('span', { class: 'badge violet' }, 'B'), methodBadge(b.method), h('span', { class: 'mono ellipsis grow' }, b.uri), h('span', { class: 'tiny muted' }, fmtTime(b.timestamp))),
          h('div', { class: 'row' }, hide.el, h('span', { class: 'grow' }),
            h('button', { class: 'btn sm', type: 'button', onclick: () => { const t = state.a; state.a = state.b; state.b = t; schedule(); renderDetail(); } }, 'Swap A and B'),
            h('button', { class: 'btn sm', type: 'button', onclick: () => compareBtn.click() }, 'Exit compare'))),
        tabs([
          { id: 'headers', label: 'Headers', render: () => tableHost },
          { id: 'body', label: 'Body', render: () => h('div', { style: { padding: '12px' } }, diffView(normaliseBody(bodyOf(a)), normaliseBody(bodyOf(b)))) },
          { id: 'line', label: 'Request line', render: () => h('div', { style: { padding: '12px' } }, diffView(line(a), line(b))) },
        ]).el);
    }

    async function clearAll() {
      const session = settings.get('session');
      const scoped = settings.instance.publicMode || settings.get('trafficSessionOnly');
      if (scoped && session) {
        const r = await send({ method: 'DELETE', url: SERVER + '/_rustybin/requests?session=' + encodeURIComponent(session), session: false });
        if (r.ok) { traffic.clearLocal(); toast(`Cleared ${r.json ? r.json.cleared : ''} requests of session ${session}`); }
        else toast(r.error || `Clear failed: ${r.status} ${(r.json && r.json.error) || ''}`, 'err');
      } else {
        const r = await send({ method: 'DELETE', url: SERVER + '/_rustybin/requests', session: false, admin: true });
        if (r.ok) { traffic.clearLocal(); toast(`Cleared ${r.json ? r.json.cleared : ''} captured requests`); }
        else if (r.status === 401 || r.status === 403) toast('Clearing every request needs the admin token (console settings), or narrow the feed to your session.', 'err');
        else toast(r.error || `Clear failed: ${r.status}`, 'err');
      }
      frozen = state.paused ? [] : null;
      state.selected = null; state.a = null; state.b = null;
      schedule();
      renderDetail();
    }

    const unsub = traffic.subscribe((kind) => {
      if (kind === 'status') { renderStatus(); return; }
      if (kind === 'add' && state.paused) { renderBanner(); return; }
      if (kind === 'add' || kind === 'clear') schedule();
    });
    const keys = (ev) => {
      if (ev.target && /INPUT|TEXTAREA|SELECT/.test(ev.target.tagName)) return;
      if (ev.key === 'j' || ev.key === 'k') {
        const list = source().filter(matches);
        const idx = list.findIndex((x) => x.id === state.selected);
        const next = list[Math.max(0, Math.min(list.length - 1, idx + (ev.key === 'j' ? 1 : -1)))];
        if (next) pick(next.id);
      } else if (ev.key === 'p' && !ev.repeat && !ev.ctrlKey && !ev.metaKey) {
        ev.preventDefault();
        pauseBtn.click();
      }
    };
    document.addEventListener('keydown', keys);
    traffic.setViewing(true);
    renderStatus();
    renderList();
    renderDetail();
    return () => {
      unsub();
      document.removeEventListener('keydown', keys);
      clearTimeout(raf);
      traffic.setViewing(false);
    };
  },
};
