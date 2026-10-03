// API explorer: the endpoint catalogue (GET /_rustybin/catalog) with search,
// category filters and a "Try it" request builder prefilled from examples.
import { h, replace, clear, card, input, textarea, select, notice, empty, icon, methodBadge, badge, spinner, debounce, copyButton } from '../lib/dom.js';
import { codeBlock } from '../lib/format.js';
import { getJson, send, toCurl, sseParser, readText } from '../lib/http.js';
import { targetPicker, responseView, headerEditor } from '../lib/widgets.js';
import * as settings from '../lib/settings.js';

let catalogCache = null;

function authHeader(auth) {
  if (!auth) return null;
  if (auth.type === 'basic') return ['Authorization', 'Basic ' + btoa(auth.username + ':' + auth.password)];
  if (auth.type === 'bearer') return ['Authorization', 'Bearer ' + auth.token];
  return null;
}

export default {
  id: 'explorer',
  mount(root, ctx) {
    let alive = true;
    let streamCtrl = null;
    const st = settings.recall('explorer.state', { q: '', cat: '', selected: null });
    settings.remember('explorer.state', st);
    const listEl = h('div', { class: 'list', role: 'listbox', 'aria-label': 'Endpoints' });
    const countEl = h('span', { class: 'muted small' });
    const detailEl = h('div', { class: 'stack' });
    const search = input({ type: 'search', placeholder: 'Search path, summary, category', value: st.q, 'aria-label': 'Search endpoints' });
    const catSel = h('select', { class: 'select', 'aria-label': 'Category' });
    const target = targetPicker('explorer');

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'API explorer'),
        h('p', null, 'Every endpoint Rustybin serves, with runnable examples. Send a request directly or through the gateway and see the status, headers, body and timing.'))));
    root.appendChild(h('div', { class: 'grid grid-sidebar' },
      h('div', { class: 'card' },
        h('div', { class: 'card-body stack-sm', style: { borderBottom: '1px solid var(--border)' } }, search, catSel, countEl),
        h('div', { class: 'pane-scroll', style: { maxHeight: 'calc(100vh - 300px)' } }, listEl)),
      detailEl));

    search.addEventListener('input', debounce(() => { st.q = search.value.trim().toLowerCase(); renderList(); }, 100));
    catSel.addEventListener('change', () => { st.cat = catSel.value; renderList(); });

    function filtered() {
      if (!catalogCache) return [];
      return catalogCache.endpoints.filter((e) => {
        if (st.cat && e.category !== st.cat) return false;
        if (!st.q) return true;
        return (e.path + ' ' + e.summary + ' ' + e.category + ' ' + e.methods.join(' ') + ' ' + e.description).toLowerCase().includes(st.q);
      });
    }

    function renderList() {
      clear(listEl);
      const list = filtered();
      countEl.textContent = `${list.length} of ${catalogCache ? catalogCache.count : 0} endpoints`;
      let lastCat = null;
      for (const e of list) {
        if (e.category !== lastCat) {
          lastCat = e.category;
          listEl.appendChild(h('div', { class: 'nav-section', style: { padding: '10px 12px 4px' } }, e.category));
        }
        listEl.appendChild(h('button', { class: 'list-item', type: 'button', role: 'option', 'aria-selected': String(e.path === st.selected), style: { gridTemplateColumns: 'auto minmax(0,1fr)' }, onclick: () => selectEp(e.path) },
          h('span', { class: 'row', style: { gap: '3px' } }, ...e.methods.slice(0, 2).map(methodBadge), e.protocol !== 'http' ? badge(e.protocol === 'sse' ? 'SSE' : 'WS', 'violet') : null),
          h('span', { style: { minWidth: 0 } }, h('div', { class: 'mono ellipsis', style: { fontWeight: 600 } }, e.path), h('div', { class: 'tiny muted ellipsis' }, e.summary))));
      }
      if (!list.length) listEl.appendChild(empty('No endpoint matches', 'Try another search term or category.'));
    }

    function selectEp(path) {
      st.selected = path;
      renderList();
      const e = catalogCache.endpoints.find((x) => x.path === path);
      if (e) renderDetail(e);
    }

    // ── Try it ──
    const methodSel = select(['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'], 'GET', { 'aria-label': 'Method', style: { width: '110px' } });
    const pathIn = input({ class: 'input mono', placeholder: '/echo', 'aria-label': 'Path and query' });
    const hdrs = headerEditor([]);
    const bodyIn = textarea({ rows: 6, placeholder: 'Request body' });
    const sendBtn = h('button', { class: 'btn primary', type: 'button', title: 'Send (Ctrl+Enter)' }, icon('send'), 'Send');
    const curlBtn = copyButton(() => toCurl(buildRequest()), { label: 'Copy as curl', toast: 'curl command copied' });
    const respEl = h('div');
    let currentEp = null;

    function buildRequest() {
      const path = pathIn.value.trim() || '/';
      return { method: methodSel.value, url: target.base() + (path.startsWith('/') ? path : '/' + path), headers: hdrs.get(), body: bodyIn.value || undefined };
    }

    function loadExample(ex) {
      methodSel.value = ex.method;
      pathIn.value = ex.path;
      const list = (ex.headers || []).map((x) => [x.name, x.value]);
      const a = authHeader(ex.auth);
      if (a) list.push(a);
      hdrs.set(list);
      bodyIn.value = ex.body ? (ex.body_type === 'json' ? prettyJson(ex.body) : ex.body) : '';
      replace(respEl);
    }

    function prettyJson(s) {
      try { return JSON.stringify(JSON.parse(s), null, 2); } catch { return s; }
    }

    async function doSend() {
      if (streamCtrl) { streamCtrl.abort(); streamCtrl = null; }
      const req = buildRequest();
      const isSse = currentEp && currentEp.protocol === 'sse' && req.method === 'GET';
      sendBtn.disabled = true;
      replace(respEl, card(null, null, spinner('Sending ' + req.method + ' ' + req.url)));
      if (isSse) {
        await doStream(req);
        sendBtn.disabled = false;
        return;
      }
      const r = await send(Object.assign({ timeout: 60000 }, req));
      sendBtn.disabled = false;
      if (!alive) return;
      replace(respEl, responseView(r));
    }

    async function doStream(req) {
      streamCtrl = new AbortController();
      const ctrl = streamCtrl;
      const r = await send(Object.assign({ stream: true, timeout: 30000, signal: ctrl.signal }, req));
      if (!alive) return;
      if (r.error || !r.response) { replace(respEl, responseView(r)); return; }
      const out = codeBlock('', null, 'tall');
      const stop = h('button', { class: 'btn sm', type: 'button', onclick: () => ctrl.abort() }, icon('stop'), 'Stop');
      const counter = h('span', { class: 'muted small' }, '0 events');
      replace(respEl, card('Event stream', { actions: h('div', { class: 'row' }, badge(String(r.status), r.ok ? 'ok' : 'err'), counter, stop) }, out));
      let n = 0;
      const feed = sseParser((ev) => {
        n++;
        counter.textContent = `${n} event${n === 1 ? '' : 's'}`;
        out.appendChild(document.createTextNode((ev.event !== 'message' ? `event: ${ev.event}\n` : '') + `data: ${ev.data}\n\n`));
        out.scrollTop = out.scrollHeight;
        if (n >= 500) ctrl.abort();
      });
      const cap = setTimeout(() => ctrl.abort(), 60000);
      try { await readText(r.response, feed, ctrl.signal); } catch { /* aborted */ }
      clearTimeout(cap);
      stop.disabled = true;
      counter.textContent += ' (stream ended)';
    }

    sendBtn.addEventListener('click', doSend);
    const keys = (e) => { if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); doSend(); } };
    root.addEventListener('keydown', keys);

    function renderDetail(e) {
      currentEp = e;
      const firstExample = e.examples[0];
      if (firstExample) loadExample(firstExample);
      else {
        methodSel.value = e.expanded_methods[0] || 'GET';
        pathIn.value = e.path.replace(/\{\*?(\w+)\}/g, '$1');
        hdrs.set([]);
        bodyIn.value = '';
        replace(respEl);
      }
      const wsUrl = e.protocol === 'websocket' ? target.base().replace(/^http/, 'ws') + e.path : null;
      replace(detailEl,
        card(null, null, h('div', { class: 'stack-sm' },
          h('div', { class: 'row' }, ...e.methods.map(methodBadge), h('h2', { class: 'mono break grow' }, e.path), badge(e.category, 'outline')),
          h('p', { style: { fontWeight: 550 } }, e.summary),
          e.description ? h('p', { class: 'small muted' }, e.description) : null,
          e.examples.length ? h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Examples'),
            h('div', { class: 'pill-list' }, ...e.examples.map((ex) => h('button', { class: 'btn sm', type: 'button', onclick: () => loadExample(ex) },
              methodBadge(ex.method), ex.name, ex.expect_status ? badge(String(ex.expect_status), 'outline') : null)))) : null,
          wsUrl ? notice('', 'WebSocket endpoint. Connect with a WebSocket client, for example ', h('code', null, `websocat ${wsUrl}`), '.') : null)),
        card('Try it', { actions: target.el }, h('div', { class: 'stack-sm' },
          h('div', { class: 'row nowrap' }, methodSel, h('div', { class: 'grow' }, pathIn), sendBtn, curlBtn),
          h('details', { class: 'disclosure', open: true }, h('summary', null, 'Headers'), hdrs.el),
          h('details', { class: 'disclosure', open: !!bodyIn.value }, h('summary', null, 'Body'), bodyIn),
          h('p', { class: 'tiny muted' }, 'Tip: add X-Rustybin-Delay: 800 or X-Rustybin-Fail: 503 to any request. Ctrl+Enter sends.'))),
        respEl);
    }

    async function load() {
      replace(listEl, h('div', { style: { padding: '12px' } }, spinner('Loading the catalogue')));
      try {
        if (!catalogCache) catalogCache = await getJson('/_rustybin/catalog', { session: false });
      } catch (e) {
        if (alive) replace(listEl, h('div', { style: { padding: '12px' } }, notice('err', 'Cannot load /_rustybin/catalog: ' + e.message, ' ', h('button', { class: 'btn sm', type: 'button', onclick: load }, 'Retry'))));
        return;
      }
      if (!alive) return;
      clear(catSel);
      catSel.appendChild(h('option', { value: '' }, 'All categories'));
      for (const c of catalogCache.categories) catSel.appendChild(h('option', { value: c.name, selected: c.name === st.cat }, `${c.name} (${c.count})`));
      renderList();
      const want = (ctx && ctx.rest) || st.selected || '/echo';
      const ep = catalogCache.endpoints.find((x) => x.path === want) || catalogCache.endpoints.find((x) => x.path === '/echo') || catalogCache.endpoints[0];
      if (ep) selectEp(ep.path);
      else replace(detailEl, empty('Empty catalogue'));
    }
    load();
    return {
      destroy: () => { alive = false; if (streamCtrl) streamCtrl.abort(); root.removeEventListener('keydown', keys); if (target.destroy) target.destroy(); },
      onRoute: (rest) => { if (rest && catalogCache) selectEp(rest); },
    };
  },
};

