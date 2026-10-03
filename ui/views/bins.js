// Request bins: create a bin with a configured response, point a client or a
// gateway's webhook / log plugin at it, and watch requests arrive live.
import { h, replace, clear, card, field, input, textarea, notice, empty, icon, methodBadge, fmtTime, fmtBytes, copyButton, toast, tabs, kvgrid, spinner, badge, debounce } from '../lib/dom.js';
import { bodyView, headersTable, codeBlock, copyableCode } from '../lib/format.js';
import { SERVER, send, toCurl } from '../lib/http.js';
import { headerEditor } from '../lib/widgets.js';
import * as settings from '../lib/settings.js';

export default {
  id: 'bins',
  mount(root) {
    let alive = true;
    let es = null;
    let selectedBin = settings.recall('bins.selected', null);
    let selectedReq = null;
    let requests = [];
    const binsEl = h('div', null, spinner('Loading bins'));
    const liveEl = h('div');
    const formErr = h('div');
    const limitsEl = h('div', { class: 'tiny muted' });

    const statusIn = input({ type: 'number', min: '200', max: '599', value: '200', class: 'input mono' });
    const delayIn = input({ type: 'number', min: '0', value: '0', class: 'input mono' });
    const bodyIn = textarea({ rows: 4, placeholder: '{"received": true}  (empty = default JSON acknowledgement)' });
    const hdrs = headerEditor([]);
    const createBtn = h('button', { class: 'btn primary', type: 'button' }, icon('plus'), 'Create bin');

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Request bins'),
        h('p', null, 'A bin records every request sent to its URL and answers with the response you configure. Point a gateway webhook, HTTP log or mirroring plugin at it, then watch the requests arrive.'))));
    root.appendChild(h('div', { class: 'grid grid-sidebar' },
      h('div', { class: 'stack' },
        card('New bin', { hint: 'POST /bin' }, h('div', { class: 'stack-sm' },
          h('div', { class: 'grid grid-2' }, field('Status', statusIn), field('Delay (ms)', delayIn)),
          field('Response body', bodyIn),
          h('div', { class: 'field' }, h('span', null, 'Response headers'), hdrs.el),
          formErr, h('div', { class: 'row' }, createBtn), limitsEl)),
        card('Your bins', { actions: h('button', { class: 'btn sm ghost icon', type: 'button', 'aria-label': 'Reload bins', onclick: loadBins }, icon('refresh')) }, binsEl)),
      liveEl));

    createBtn.addEventListener('click', async () => {
      replace(formErr);
      const payload = {};
      const status = Number(statusIn.value);
      if (status) payload.status = status;
      const delay = Number(delayIn.value);
      if (delay) payload.delay_ms = delay;
      const headers = hdrs.get();
      if (headers.length) payload.headers = Object.fromEntries(headers);
      const body = bodyIn.value;
      if (body.trim()) {
        try { payload.body = JSON.parse(body); } catch { payload.body = body; }
      }
      createBtn.disabled = true;
      const r = await send({ method: 'POST', url: SERVER + '/bin', headers: [['Content-Type', 'application/json']], body: JSON.stringify(payload) });
      createBtn.disabled = false;
      if (!alive) return;
      if (r.status === 201 && r.json) {
        toast('Bin created');
        selectedBin = r.json.id;
        await loadBins();
        openBin(r.json.id);
      } else {
        replace(formErr, notice('err', r.error || (r.json && r.json.error) || `POST /bin answered ${r.status}`));
      }
    });

    async function loadBins() {
      const r = await send({ url: SERVER + '/bin', timeout: 10000 });
      if (!alive) return;
      if (r.error || !r.json) { replace(binsEl, notice('err', r.error || `GET /bin answered ${r.status}`)); return; }
      const l = r.json.limits;
      if (l) limitsEl.textContent = `Limits: ${l.max_bins_per_session ?? l.max_bins} bins, ${l.max_requests} requests each, expire after ${Math.round((l.ttl_secs || 0) / 60)} min.`;
      const bins = r.json.bins || [];
      if (!bins.length) { replace(binsEl, empty('No bins yet', 'Create one above. Bins belong to your session (or IP) and expire automatically.', 'bin')); return; }
      replace(binsEl, h('div', { class: 'list', style: { margin: '-14px -16px' } }, ...bins.map((b) =>
        h('button', { class: 'list-item', type: 'button', 'aria-selected': String(b.id === selectedBin), style: { gridTemplateColumns: 'minmax(0,1fr) auto auto' }, onclick: () => openBin(b.id) },
          h('span', { class: 'mono ellipsis' }, b.id),
          badge(`${b.total_requests} req`, b.total_requests ? 'info' : ''),
          h('span', { class: 'st st-' + Math.floor(b.response.status / 100) }, String(b.response.status))))));
      if (selectedBin && !bins.some((b) => b.id === selectedBin)) { selectedBin = null; renderLive(); }
      binsEl.append(h('p', { class: 'tiny muted', style: { padding: '24px 0 0', margin: 0 } }, 'Bins belong to your session (or client IP) and expire automatically.'));
    }

    const refreshBinsSoon = debounce(() => { if (alive) loadBins(); }, 400);

    function closeStream() {
      if (es) { es.close(); es = null; }
    }

    async function openBin(id) {
      closeStream();
      selectedBin = id;
      settings.remember('bins.selected', id);
      selectedReq = null;
      requests = [];
      loadBins();
      replace(liveEl, card('Bin ' + id, null, spinner('Loading requests')));
      const r = await send({ url: SERVER + '/bin/' + id + '/requests?limit=100', timeout: 10000 });
      if (!alive || selectedBin !== id) return;
      if (r.error || !r.json) {
        replace(liveEl, card('Bin ' + id, null, notice('err', r.error || (r.json && r.json.error) || `answered ${r.status}`)));
        return;
      }
      requests = (r.json.requests || []).slice();
      renderLive(r.json);
      es = new EventSource(SERVER + '/bin/' + id + '/requests/stream');
      es.addEventListener('request', (ev) => {
        try {
          const entry = JSON.parse(ev.data);
          if (requests.some((x) => x.seq === entry.seq)) return;
          requests.unshift(entry);
          if (requests.length > 200) requests.length = 200;
          if (!selectedReq) selectedReq = entry.seq;
          renderRequests();
          refreshBinsSoon();
        } catch { /* ignore */ }
      });
      es.addEventListener('error', () => { if (es && es.readyState === EventSource.CLOSED) liveDot.className = 'dot err'; });
      es.addEventListener('open', () => { liveDot.className = 'dot live'; });
    }

    const liveDot = h('span', { class: 'dot warn' });
    const reqList = h('div', { class: 'list' });
    const reqDetail = h('div');

    function renderLive(data) {
      if (!selectedBin) {
        replace(liveEl, card(null, null, empty('No bin selected', 'Create a bin or pick one from the list.', 'inbox')));
        return;
      }
      const id = selectedBin;
      const url = SERVER + '/bin/' + id;
      const sample = toCurl({ method: 'POST', url: url + '/webhook', headers: [['Content-Type', 'application/json']], body: '{"event":"order.created","id":42}' });
      const resp = data && data.bin ? data.bin.response : null;
      replace(liveEl, h('div', { class: 'stack' },
        card('Bin', { actions: h('div', { class: 'row' }, h('span', { class: 'row small muted', style: { gap: '6px' } }, liveDot, 'live'),
          h('button', { class: 'btn sm', type: 'button', onclick: sendTest }, icon('send'), 'Send test request'),
          h('button', { class: 'btn sm danger', type: 'button', onclick: deleteBin }, icon('trash'), 'Delete')) },
        h('div', { class: 'stack-sm' },
          h('div', { class: 'row nowrap' }, h('code', { class: 'grow break', style: { fontSize: '14px' } }, url), copyButton(url, { label: 'Copy URL' })),
          h('p', { class: 'small muted' }, 'Any method and any sub path is captured (', h('code', null, '/bin/' + id + '/anything'), '). Requests are listed below as they arrive.'),
          resp ? kvgrid([['Responds with', `${resp.status}${resp.delay_ms ? ` after ${resp.delay_ms} ms` : ''}${resp.body ? ', custom body' : ''}${resp.headers && resp.headers.length ? `, ${resp.headers.length} header(s)` : ''}`], ['Expires', data.bin.expires_at ? new Date(data.bin.expires_at).toLocaleString() : undefined]]) : null,
          h('details', { class: 'disclosure' }, h('summary', null, 'Example curl'), copyableCode(sample)))),
        h('div', { class: 'card' }, h('div', { class: 'split', style: { minHeight: '360px' } },
          h('div', { class: 'pane' }, h('div', { class: 'pane-scroll' }, reqList)),
          h('div', { class: 'pane' }, h('div', { class: 'pane-scroll' }, reqDetail))))));
      renderRequests();
    }

    function renderRequests() {
      clear(reqList);
      if (!requests.length) {
        reqList.appendChild(empty('No requests yet', 'Send something to the bin URL (or press "Send test request").', 'inbox'));
      }
      for (const r of requests) {
        reqList.appendChild(h('button', { class: 'list-item', type: 'button', 'aria-selected': String(r.seq === selectedReq), style: { gridTemplateColumns: '36px 62px minmax(0,1fr) 76px' }, onclick: () => { selectedReq = r.seq; renderRequests(); } },
          h('span', { class: 'mono tiny muted' }, '#' + r.seq), methodBadge(r.method), h('span', { class: 'mono ellipsis' }, r.path + (r.query ? '?' + r.query : '')),
          h('span', { class: 'mono tiny muted' }, fmtTime(r.timestamp).slice(0, 8))));
      }
      const cur = requests.find((x) => x.seq === selectedReq) || requests[0];
      if (!cur) { replace(reqDetail); return; }
      selectedReq = cur.seq;
      replace(reqDetail, h('div', null,
        h('div', { class: 'row', style: { padding: '10px 14px', borderBottom: '1px solid var(--border)' } }, methodBadge(cur.method), h('span', { class: 'mono break grow' }, cur.full_path + (cur.query ? '?' + cur.query : '')),
          h('span', { class: 'tiny muted' }, cur.client_ip || ''), h('span', { class: 'tiny muted' }, fmtBytes(cur.body_size))),
        tabs([
          { id: 'body', label: 'Body', render: () => h('div', { style: { padding: '12px' } }, cur.body_base64 ? codeBlock(cur.body_base64) : bodyView(cur.body, cur.content_type, { cls: 'tall' })) },
          { id: 'headers', label: `Headers (${cur.headers.length})`, render: () => headersTable(cur.headers) },
        ]).el));
    }

    async function sendTest() {
      const id = selectedBin;
      if (!id) return;
      const r = await send({ method: 'POST', url: SERVER + '/bin/' + id + '/webhook?source=console', headers: [['Content-Type', 'application/json'], ['X-Demo-Event', 'order.created']], body: JSON.stringify({ event: 'order.created', id: 42, amount: { value: 1999, currency: 'EUR' }, at: new Date().toISOString() }) });
      if (r.error) toast(r.error, 'err');
      else toast(`Bin answered ${r.status}`);
    }

    async function deleteBin() {
      const id = selectedBin;
      if (!id) return;
      const r = await send({ method: 'DELETE', url: SERVER + '/bin/' + id });
      if (r.ok) { toast('Bin deleted'); closeStream(); selectedBin = null; settings.remember('bins.selected', null); renderLive(); loadBins(); }
      else toast(r.error || `Delete failed: ${r.status}`, 'err');
    }

    loadBins().then(() => { if (selectedBin) openBin(selectedBin); else renderLive(); });
    return () => { alive = false; closeStream(); };
  },
};
