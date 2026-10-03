// Chaos and health: health toggle, flaky endpoints with live counters, a
// fault-injection header builder and a small capped load generator (status
// histogram and latency percentiles) for rate limiting, retry and circuit
// breaker demos through a gateway.
import { h, s, replace, clear, card, field, input, select, notice, icon, badge, fmtMs, spinner, toast, segmented } from '../lib/dom.js';
import { copyableCode } from '../lib/format.js';
import { SERVER, send, toCurl, getJson } from '../lib/http.js';
import { targetPicker, responseView } from '../lib/widgets.js';
import * as settings from '../lib/settings.js';

const MAX_REQUESTS = 500;
const MAX_CONCURRENCY = 50;
const STATUS_COLORS = { '2': 'var(--status-good)', '3': 'var(--info)', '4': 'var(--status-warning)', '5': 'var(--status-critical)', '0': 'var(--status-serious)' };
const STATUS_NAMES = { '2': '2xx success', '3': '3xx redirect', '4': '4xx client error', '5': '5xx server error', '0': 'no response' };

const FLAKY_PRESETS = [
  ['/flaky/50', '50% failures (random)'],
  ['/flaky/pattern/SSFSS', 'Pattern SSFSS (deterministic)'],
  ['/flaky/after/3', 'Fail after 3 successes (trip a breaker)'],
  ['/flaky/recover/3', 'Recover after 3 failures (half-open)'],
];

export function percentile(sorted, p) {
  if (!sorted.length) return 0;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

function tip() {
  let el = document.querySelector('.chart-tip');
  if (!el) { el = h('div', { class: 'chart-tip', hidden: true }); document.body.appendChild(el); }
  return el;
}
function showTip(ev, text) {
  const t = tip();
  t.textContent = text;
  t.hidden = false;
  t.style.left = Math.min(window.innerWidth - 180, ev.clientX + 12) + 'px';
  t.style.top = ev.clientY + 12 + 'px';
}
function hideTip() { tip().hidden = true; }

export default {
  id: 'chaos',
  mount(root) {
    let alive = true;
    let healthTimer = null;
    let loadCtrl = null;
    const st = settings.recall('chaos.state', { flaky: '/flaky/after/3', results: [], path: '/echo', delay: '', failStatus: '', failPct: '', lgPath: '/flaky/50', lgMethod: 'GET', lgN: 100, lgC: 10, lgFaults: false, lgRun: null });
    settings.remember('chaos.state', st);
    const flakyTarget = targetPicker('chaos-flaky');
    const faultTarget = targetPicker('chaos-fault');
    const loadTarget = targetPicker('chaos-load');

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Chaos and health'),
        h('p', null, 'Make the upstream misbehave on purpose: flip health checks, trip circuit breakers, inject latency and errors, and fire bursts of traffic to show rate limiting and retries at the gateway.'))));

    // ── Health ──
    const healthEl = h('div', null, spinner('Reading the health state'));
    const healthMsg = h('div');
    async function refreshHealth() {
      try {
        const stt = await getJson('/_rustybin/status', { session: false });
        if (!alive) return;
        replace(healthEl, h('div', { class: 'row' },
          h('span', { class: 'dot ' + (stt.healthy ? 'ok' : 'err'), style: { width: '14px', height: '14px' } }),
          h('strong', { style: { fontSize: '20px' } }, stt.healthy ? 'Healthy' : 'Unhealthy'),
          h('span', { class: 'muted small' }, stt.healthy ? 'GET /health answers 200, gRPC health SERVING' : 'GET /health answers 503, gRPC health NOT_SERVING')));
      } catch (e) {
        if (alive) replace(healthEl, notice('err', e.message));
      }
    }
    async function setHealth(action) {
      const r = await send({ method: 'POST', url: SERVER + '/health/' + action, admin: true, timeout: 10000 });
      if (r.ok) { toast(`Health: ${r.json && r.json.status}`); replace(healthMsg); }
      else if (r.status === 401 || r.status === 403) replace(healthMsg, notice('err', (r.json && r.json.error) || 'Admin token required', ' Set it in the console settings.'));
      else replace(healthMsg, notice('err', r.error || `answered ${r.status}`));
      refreshHealth();
    }

    // ── Flaky ──
    const flakySel = select(FLAKY_PRESETS, st.flaky, { 'aria-label': 'Flaky endpoint' });
    flakySel.addEventListener('change', () => { st.flaky = flakySel.value; });
    const flakyStats = h('div');
    const strip = h('div', { class: 'row', style: { gap: '3px', minHeight: '18px' } });
    const serverCounters = h('div', { class: 'small muted' });
    async function fire(n) {
      for (let i = 0; i < n && alive; i++) {
        const r = await send({ url: flakyTarget.base() + st.flaky, timeout: 15000 });
        st.results.push({ path: st.flaky, status: r.status, ms: r.ms, n: r.header('x-rustybin-request-number') });
        if (st.results.length > 200) st.results.shift();
        renderFlaky();
      }
      refreshCounters();
    }
    function renderFlaky() {
      const ok = st.results.filter((r) => r.status >= 200 && r.status < 400).length;
      const fail = st.results.length - ok;
      replace(flakyStats, h('div', { class: 'row', style: { gap: '18px' } },
        h('div', null, h('div', { class: 'label' }, 'Success'), h('div', { style: { fontSize: '26px', fontWeight: 700, color: 'var(--ok)' } }, String(ok))),
        h('div', null, h('div', { class: 'label' }, 'Failure'), h('div', { style: { fontSize: '26px', fontWeight: 700, color: 'var(--err)' } }, String(fail))),
        h('div', null, h('div', { class: 'label' }, 'Failure rate'), h('div', { style: { fontSize: '26px', fontWeight: 700 } }, st.results.length ? Math.round((fail / st.results.length) * 100) + '%' : '0%'))));
      clear(strip);
      for (const r of st.results.slice(-60)) {
        const good = r.status >= 200 && r.status < 400;
        strip.appendChild(h('span', { title: `${r.path} #${r.n || '?'}: ${r.status || 'error'} in ${fmtMs(r.ms)}`, style: { width: '10px', height: '18px', borderRadius: '2px', background: good ? 'var(--status-good)' : 'var(--status-critical)' } }));
      }
    }
    async function refreshCounters() {
      const r = await send({ url: flakyTarget.base() + '/flaky/status', timeout: 8000 });
      if (!alive) return;
      if (r.json && r.json.counters) {
        serverCounters.textContent = r.json.counters.length
          ? `Server counters for session ${r.json.session}: ` + r.json.counters.map((c) => `${c.route} = ${c.count}`).join(', ')
          : `No server counters yet for session ${r.json.session}.`;
      } else serverCounters.textContent = r.error || '';
    }
    async function resetFlaky() {
      const r = await send({ method: 'POST', url: flakyTarget.base() + '/flaky/reset', timeout: 8000 });
      st.results = [];
      renderFlaky();
      refreshCounters();
      toast(r.ok ? `Reset ${r.json ? r.json.counters_removed : ''} counters` : (r.error || `Reset failed: ${r.status}`), r.ok ? undefined : 'err');
    }

    // ── Fault builder ──
    const fPath = input({ value: st.path, class: 'input mono' });
    const fDelay = input({ type: 'number', min: '0', value: st.delay, placeholder: '0', class: 'input mono' });
    const fStatus = select([['', 'none'], '429', '500', '502', '503', '504', '400', '401', '403', '404'], st.failStatus);
    const fPct = input({ type: 'number', min: '1', max: '100', value: st.failPct, placeholder: '100', class: 'input mono' });
    const curlHost = h('div');
    const faultResp = h('div');
    function faultHeaders() {
      const hs = [];
      if (Number(fDelay.value) > 0) hs.push(['X-Rustybin-Delay', String(Number(fDelay.value))]);
      if (fStatus.value) hs.push(['X-Rustybin-Fail', fStatus.value + (fPct.value && Number(fPct.value) < 100 ? ':' + Number(fPct.value) : '')]);
      return hs;
    }
    function renderCurl() {
      st.path = fPath.value.trim() || '/echo';
      st.delay = fDelay.value; st.failStatus = fStatus.value; st.failPct = fPct.value;
      const req = { method: 'GET', url: faultTarget.base() + (st.path.startsWith('/') ? st.path : '/' + st.path), headers: faultHeaders() };
      replace(curlHost, copyableCode(toCurl(req)));
      return req;
    }
    for (const el of [fPath, fDelay, fPct]) el.addEventListener('input', renderCurl);
    fStatus.addEventListener('change', renderCurl);

    // ── Load generator ──
    const lgPath = input({ value: st.lgPath, class: 'input mono', 'aria-label': 'Path' });
    const lgMethod = select(['GET', 'POST', 'PUT', 'DELETE'], st.lgMethod, { 'aria-label': 'Method', style: { width: '100px' } });
    const lgN = input({ type: 'number', min: '1', max: String(MAX_REQUESTS), value: String(st.lgN), class: 'input mono' });
    const lgC = input({ type: 'number', min: '1', max: String(MAX_CONCURRENCY), value: String(st.lgC), class: 'input mono' });
    const lgFaults = segmented([['off', 'Off'], ['on', 'Include them']], st.lgFaults ? 'on' : 'off', (v) => { st.lgFaults = v === 'on'; }, 'Fault headers');
    const runBtn = h('button', { class: 'btn primary', type: 'button' }, icon('play'), 'Run');
    const stopBtn = h('button', { class: 'btn', type: 'button', hidden: true }, icon('stop'), 'Stop');
    const lgOut = h('div');
    const presets = h('div', { class: 'pill-list' }, ...[['/flaky/30', 'retries'], ['/status/429', '429 storm'], ['/delay/800', 'slow upstream'], ['/echo', 'rate limit'], ['/flaky/after/20', 'breaker trip']].map(([p, label]) =>
      h('button', { class: 'btn sm', type: 'button', onclick: () => { lgPath.value = p; } }, label, h('code', { class: 'tiny muted' }, ' ' + p))));

    async function runLoad() {
      const n = Math.min(MAX_REQUESTS, Math.max(1, Math.floor(Number(lgN.value) || 1)));
      const c = Math.min(MAX_CONCURRENCY, Math.max(1, Math.floor(Number(lgC.value) || 1)));
      lgN.value = String(n); lgC.value = String(c);
      st.lgN = n; st.lgC = c; st.lgPath = lgPath.value.trim() || '/'; st.lgMethod = lgMethod.value;
      const url = loadTarget.base() + (st.lgPath.startsWith('/') ? st.lgPath : '/' + st.lgPath);
      const headers = st.lgFaults ? faultHeaders() : [];
      loadCtrl = new AbortController();
      const run = { url, method: st.lgMethod, n, c, results: [], started: performance.now(), done: false, headers };
      st.lgRun = run;
      runBtn.hidden = true; stopBtn.hidden = false;
      let next = 0;
      let lastPaint = 0;
      const worker = async () => {
        while (next < n && !loadCtrl.signal.aborted && alive) {
          const i = next++;
          const t0 = performance.now() - run.started;
          const r = await send({ method: run.method, url, headers, timeout: 30000, signal: loadCtrl.signal });
          if (loadCtrl.signal.aborted && !r.status) break;
          run.results.push({ i, status: r.status, ms: r.ms, at: t0, error: r.error });
          if (performance.now() - lastPaint > 150) { lastPaint = performance.now(); renderLoad(); }
        }
      };
      await Promise.all(Array.from({ length: c }, worker));
      run.done = true;
      run.elapsed = performance.now() - run.started;
      runBtn.hidden = false; stopBtn.hidden = true;
      loadCtrl = null;
      if (alive) renderLoad();
    }
    runBtn.addEventListener('click', runLoad);
    stopBtn.addEventListener('click', () => { if (loadCtrl) loadCtrl.abort(); });

    function renderLoad() {
      const run = st.lgRun;
      if (!run) { replace(lgOut, h('p', { class: 'small muted' }, `Capped at ${MAX_REQUESTS} requests and ${MAX_CONCURRENCY} in flight (browsers also limit parallel connections per host).`)); return; }
      const res = run.results;
      const lat = res.filter((r) => r.status).map((r) => r.ms).sort((a, b) => a - b);
      const elapsed = (run.elapsed || performance.now() - run.started) / 1000;
      const byStatus = new Map();
      for (const r of res) byStatus.set(r.status, (byStatus.get(r.status) || 0) + 1);
      const ok = res.filter((r) => r.status >= 200 && r.status < 400).length;
      const firstError = res.find((r) => r.error);
      const statTile = (label, value) => h('div', { class: 'card tile' }, h('span', { class: 'label' }, label), h('div', { class: 'value', style: { fontSize: '22px' } }, value));
      replace(lgOut, h('div', { class: 'stack' },
        h('div', { class: 'row' }, run.done ? badge('finished', 'ok') : h('span', { class: 'row small' }, h('span', { class: 'spinner' }), 'running'),
          h('span', { class: 'small muted mono ellipsis grow' }, `${run.method} ${run.url}`), h('span', { class: 'small' }, `${res.length} / ${run.n} sent`)),
        h('div', { class: 'progress' }, h('div', { style: { width: (res.length / run.n) * 100 + '%' } })),
        firstError ? notice('err', firstError.error) : null,
        h('div', { class: 'grid grid-4' },
          statTile('Success', `${ok} / ${res.length}`), statTile('Throughput', (res.length / Math.max(elapsed, 0.001)).toFixed(1) + ' req/s'),
          statTile('p50 / p95', `${fmtMs(percentile(lat, 50))} / ${fmtMs(percentile(lat, 95))}`), statTile('p99 / max', `${fmtMs(percentile(lat, 99))} / ${fmtMs(lat[lat.length - 1] || 0)}`)),
        h('div', { class: 'grid grid-2' },
          h('div', { class: 'stack-sm' }, h('h3', null, 'Responses by status'), histogram(byStatus, res.length)),
          h('div', { class: 'stack-sm' }, h('h3', null, 'Latency per request'), scatter(res), legend(res)))));
    }

    function histogram(byStatus, total) {
      const rows = Array.from(byStatus.entries()).sort((a, b) => a[0] - b[0]);
      if (!rows.length) return h('p', { class: 'small muted' }, 'No responses yet.');
      const max = Math.max(...rows.map(([, n]) => n));
      const W = 480, rowH = 26, labelW = 64, countW = 70;
      const svg = s('svg', { class: 'chart', viewBox: `0 0 ${W} ${rows.length * rowH + 4}`, role: 'img', 'aria-label': 'Responses by status code' });
      rows.forEach(([code, n], i) => {
        const y = i * rowH + 4;
        const w = Math.max(3, ((W - labelW - countW) * n) / max);
        const cls = String(Math.floor(code / 100));
        svg.appendChild(s('text', { x: 0, y: y + 15 }, code ? String(code) : 'error'));
        svg.appendChild(s('rect', { x: labelW, y: y + 2, width: w, height: rowH - 8, rx: 4, fill: STATUS_COLORS[cls] || 'var(--text-3)',
          onmousemove: (ev) => showTip(ev, `${code || 'no response'}: ${n} (${Math.round((n / total) * 100)}%)`), onmouseleave: hideTip }));
        svg.appendChild(s('text', { x: labelW + w + 6, y: y + 15 }, `${n} (${Math.round((n / total) * 100)}%)`));
      });
      const table = h('table', { class: 'table sr-only' }, h('caption', null, 'Responses by status'), h('tbody', null, ...rows.map(([code, n]) => h('tr', null, h('td', null, String(code || 'error')), h('td', null, String(n))))));
      return h('div', null, svg, table);
    }

    function scatter(res) {
      if (!res.length) return h('p', { class: 'small muted' }, 'No responses yet.');
      const W = 480, H = 200, padL = 52, padB = 22, padT = 8;
      const maxMs = Math.max(10, ...res.map((r) => r.ms));
      const maxAt = Math.max(1, ...res.map((r) => r.at + r.ms));
      const svg = s('svg', { class: 'chart', viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': 'Latency of each request over time' });
      for (let k = 0; k <= 4; k++) {
        const y = padT + ((H - padT - padB) * k) / 4;
        svg.appendChild(s('line', { x1: padL, x2: W, y1: y, y2: y, class: 'grid-line' }));
        svg.appendChild(s('text', { x: padL - 6, y: y + 4, 'text-anchor': 'end' }, Math.round(maxMs * (1 - k / 4)) + ' ms'));
      }
      svg.appendChild(s('text', { x: W, y: H - 4, 'text-anchor': 'end' }, `${(maxAt / 1000).toFixed(1)} s`));
      svg.appendChild(s('text', { x: padL, y: H - 4 }, '0 s'));
      for (const r of res) {
        const x = padL + ((W - padL - 6) * r.at) / maxAt;
        const y = padT + (H - padT - padB) * (1 - r.ms / maxMs);
        const cls = String(Math.floor((r.status || 0) / 100));
        svg.appendChild(s('circle', { cx: x.toFixed(1), cy: y.toFixed(1), r: 4, fill: STATUS_COLORS[cls] || 'var(--text-3)', stroke: 'var(--surface)', 'stroke-width': 1.5,
          onmousemove: (ev) => showTip(ev, `#${r.i + 1}: ${r.status || 'error'} in ${fmtMs(r.ms)}`), onmouseleave: hideTip }));
      }
      return svg;
    }

    function legend(res) {
      const classes = Array.from(new Set(res.map((r) => String(Math.floor((r.status || 0) / 100))))).sort();
      return h('div', { class: 'legend' }, ...classes.map((c) => h('span', null, h('i', { style: { background: STATUS_COLORS[c] } }), STATUS_NAMES[c] || c)));
    }

    // ── Layout ──
    root.appendChild(h('div', { class: 'stack' },
      h('div', { class: 'grid grid-2' },
        card('Health check', { hint: '/health (instance global)' }, h('div', { class: 'stack-sm' }, healthEl,
          h('div', { class: 'row' },
            h('button', { class: 'btn', type: 'button', onclick: () => setHealth('healthy') }, h('span', { class: 'dot ok' }), 'Mark healthy'),
            h('button', { class: 'btn', type: 'button', onclick: () => setHealth('unhealthy') }, h('span', { class: 'dot err' }), 'Mark unhealthy'),
            h('button', { class: 'btn', type: 'button', onclick: () => setHealth('toggle') }, icon('refresh'), 'Toggle')),
          healthMsg,
          h('p', { class: 'tiny muted' }, 'Point the gateway\'s active health check at /health to show failover and recovery. Needs the admin token when one is configured.'))),
        card('Flaky upstream', { actions: flakyTarget.el }, h('div', { class: 'stack-sm' },
          h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, flakySel),
            h('button', { class: 'btn primary', type: 'button', onclick: () => fire(1) }, 'Send 1'),
            h('button', { class: 'btn', type: 'button', onclick: () => fire(10) }, 'Send 10'),
            h('button', { class: 'btn ghost', type: 'button', onclick: resetFlaky }, 'Reset')),
          flakyStats, strip, serverCounters))),
      card('Fault injection headers', { hint: 'work on every route', actions: faultTarget.el }, h('div', { class: 'stack-sm' },
        h('div', { class: 'grid grid-4' }, field('Path', fPath), field('X-Rustybin-Delay (ms)', fDelay), field('X-Rustybin-Fail status', fStatus), field('Failure percent', fPct, 'blank = always')),
        curlHost,
        h('div', { class: 'row' }, h('button', { class: 'btn', type: 'button', onclick: async () => { const req = renderCurl(); replace(faultResp, spinner('Sending')); const r = await send(Object.assign({ timeout: 60000 }, req)); if (alive) replace(faultResp, responseView(r)); } }, icon('send'), 'Send once')),
        faultResp)),
      card('Load generator', { actions: loadTarget.el }, h('div', { class: 'stack-sm' },
        h('div', { class: 'row nowrap' }, lgMethod, h('div', { class: 'grow' }, lgPath)),
        presets,
        h('div', { class: 'grid grid-4' }, field('Requests (max 500)', lgN), field('Concurrency (max 50)', lgC), h('div', { class: 'field' }, h('span', null, 'Fault headers from above'), lgFaults.el),
          h('div', { class: 'row', style: { alignItems: 'flex-end' } }, runBtn, stopBtn)),
        lgOut))));

    renderFlaky();
    renderCurl();
    renderLoad();
    refreshHealth();
    refreshCounters();
    healthTimer = setInterval(refreshHealth, 4000);
    return () => {
      alive = false;
      clearInterval(healthTimer);
      if (loadCtrl) loadCtrl.abort();
      hideTip();
      for (const t of [flakyTarget, faultTarget, loadTarget]) if (t.destroy) t.destroy();
    };
  },
};

