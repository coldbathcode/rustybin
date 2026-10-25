// Harness: this Rustybin as a load client. Node status and caps, a scenario
// builder generated from the server's JSON Schema (with a YAML view and the
// built-in scenarios), validation, a live run view with a per-second chart,
// the kill switch, and the final report with its thresholds.
import { h, s, replace, clear, card, kvgrid, notice, badge, spinner, toast, select, textarea, fmtMs, copyButton } from '../lib/dom.js';
import { jsonView } from '../lib/format.js';
import { SERVER, send, getJson } from '../lib/http.js';
import { scenarioForm, toYaml } from '../lib/scenarioform.js';
import * as settings from '../lib/settings.js';

const PREFIX = '/_rustybin/harness';

function tile(label, value, cls) {
  return h('div', { class: 'tile' }, h('div', { class: 'label' }, label), h('div', { class: 'mono ' + (cls || ''), style: { fontSize: '22px', fontWeight: 700 } }, value));
}

function num(v, digits = 0) {
  if (v === undefined || v === null) return '-';
  return Number(v).toLocaleString(undefined, { maximumFractionDigits: digits });
}

// A per-second chart: completed per second (bars), p99 latency (line).
function timelineChart(buckets) {
  const W = 640, H = 160, pad = 28;
  if (!buckets.length) return h('p', { class: 'muted small' }, 'No data yet.');
  const maxRate = Math.max(1, ...buckets.map((b) => Math.max(b.requested || 0, b.completed || 0)));
  const maxP99 = Math.max(1, ...buckets.map((b) => b.p99_ms || 0));
  const n = buckets.length;
  const bw = (W - pad * 2) / n;
  const svg = s('svg', { class: 'chart', viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': 'Answers per second and p99 latency' });
  svg.appendChild(s('line', { x1: pad, y1: H - pad, x2: W - pad, y2: H - pad, stroke: 'var(--border)' }));
  buckets.forEach((b, i) => {
    const x = pad + i * bw;
    const hr = ((b.completed || 0) / maxRate) * (H - pad * 2);
    svg.appendChild(s('rect', { x: x + 1, y: H - pad - hr, width: Math.max(1, bw - 2), height: hr, fill: b.saturated ? 'var(--status-critical)' : b.errors ? 'var(--status-warning)' : 'var(--status-good)' }, s('title', null, `${b.t_secs}s: ${b.completed} answered, ${b.errors} errors, ${b.not_sent} not sent, p99 ${fmtMs(b.p99_ms)}`)));
    const hq = ((b.requested || 0) / maxRate) * (H - pad * 2);
    svg.appendChild(s('line', { x1: x + 1, y1: H - pad - hq, x2: x + bw - 1, y2: H - pad - hq, stroke: 'var(--text-3)', 'stroke-dasharray': '2 2' }));
  });
  const pts = buckets.map((b, i) => `${pad + i * bw + bw / 2},${H - pad - ((b.p99_ms || 0) / maxP99) * (H - pad * 2)}`).join(' ');
  svg.appendChild(s('polyline', { points: pts, fill: 'none', stroke: 'var(--info)', 'stroke-width': 2 }));
  svg.appendChild(s('text', { x: pad, y: 12, fill: 'var(--text-3)', 'font-size': 10 }, `answers/s (max ${num(maxRate)}), dashed: requested`));
  svg.appendChild(s('text', { x: W - pad, y: 12, fill: 'var(--info)', 'font-size': 10, 'text-anchor': 'end' }, `p99 latency (max ${fmtMs(maxP99)})`));
  return svg;
}

export default {
  id: 'harness',
  mount(root) {
    let alive = true;
    let pollTimer = null;
    let liveBuckets = [];
    const st = settings.recall('harness.state', { runId: null, scenarioText: '', builtin: 'smoke' });
    settings.remember('harness.state', st);

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Harness'),
        h('p', null, 'Rustybin as the load client: send a shaped load from this node to a gateway (or back to itself), with every target allowlisted and the client watching itself for saturation, so a slow client is never read as a slow gateway.'))));

    const nodeEl = h('div', null, spinner('Reading the node status'));
    const builderEl = h('div', null, spinner('Loading the scenario schema'));
    const runEl = h('div', null, h('p', { class: 'muted small' }, 'No run yet. Validate a scenario and start it.'));
    const reportEl = h('div', null, h('p', { class: 'muted small' }, 'The report appears when a run finishes.'));
    const killMsg = h('div');
    const killBtn = h('button', { class: 'btn sm', type: 'button', id: 'harness-kill', onclick: killSwitch }, 'Kill switch: stop all load');

    root.appendChild(h('div', { class: 'grid grid-2' },
      card('Node', { hint: 'GET /_rustybin/harness', actions: h('div', { class: 'row' }, killBtn) }, nodeEl, killMsg),
      card('Run', { hint: 'live, one second at a time' }, runEl)));
    root.appendChild(card('Scenario', { hint: 'generated from GET /_rustybin/harness/schema' }, builderEl));
    root.appendChild(card('Report', { hint: 'thresholds, totals, latency, timeline' }, reportEl));

    // ── Node ──
    async function loadNode() {
      const r = await send({ url: SERVER + PREFIX, timeout: 10000 });
      if (!alive) return null;
      if (!r.ok || !r.json) {
        replace(nodeEl, notice('err', r.error || `GET ${PREFIX} answered ${r.status}`));
        return null;
      }
      const n = r.json;
      if (!n.enabled) {
        replace(nodeEl, notice('warn', 'Harness mode is off on this instance. Start it with RUSTYBIN_HARNESS=on, RUSTYBIN_HARNESS_ROLES=both (loop-through) or client, and RUSTYBIN_HARNESS_TARGETS listing the hosts it may send load to.'));
        return n;
      }
      const caps = n.caps || {};
      replace(nodeEl, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, ...(n.roles || []).map((x) => badge(x, 'violet')), n.kill_switch ? badge('kill switch set', 'err') : null, n.active_run ? badge('run ' + n.active_run.run_id + ' ' + n.active_run.state, 'ok') : badge('idle', 'outline')),
        kvgrid([
          ['Node id', h('code', null, n.node_id)],
          ['Caps', `${num(caps.max_rate)} requests/s, ${num(caps.max_inflight)} in flight, ${num(caps.max_duration_secs)} s`],
          ['Engine', n.engine ? `${n.engine.threads} thread(s), in-flight cap ${num(n.engine.inflight_cap)} (descriptor limit ${n.engine.descriptor_limit ?? 'unknown'})` : 'none (no client role)'],
          ['Allowed targets', n.allowlist && n.allowlist.length ? n.allowlist.join(', ') : 'none: set RUSTYBIN_HARNESS_TARGETS'],
          ['Verify targets', n.verify_targets ? 'on (targets serve /.well-known/rustybin-harness)' : 'off'],
          ['Runs kept', String(n.runs || 0)],
        ]),
        n.engine_error ? notice('err', n.engine_error) : null));
      if (n.active_run && !st.runId) {
        st.runId = n.active_run.run_id;
        startPolling();
      }
      return n;
    }

    async function killSwitch() {
      const r = await send({ method: 'POST', url: SERVER + PREFIX + '/stop', admin: true, timeout: 10000 });
      if (r.ok) { toast('All load stopped'); replace(killMsg); }
      else replace(killMsg, notice('err', (r.json && r.json.error) || r.error || `answered ${r.status}`));
      loadNode();
    }

    // ── Scenario builder ──
    let form = null;
    let schema = null;
    let yamlArea = null;
    let builtinSel = null;
    const problemsEl = h('div');
    const summaryEl = h('div', { class: 'small muted' });

    async function loadBuilder() {
      const [sr, lr] = await Promise.all([
        send({ url: SERVER + PREFIX + '/schema', timeout: 10000 }),
        send({ url: SERVER + PREFIX + '/scenarios', timeout: 10000 }),
      ]);
      if (!alive) return;
      if (!sr.ok || !sr.json) {
        replace(builderEl, notice('err', sr.error || `GET ${PREFIX}/schema answered ${sr.status}`));
        return;
      }
      schema = sr.json;
      const builtins = (lr.json && lr.json.scenarios) || [];
      builtinSel = select([['', 'Built-in scenarios...'], ...builtins.map((b) => [b.name, `${b.name}: ${b.description || ''}`.slice(0, 90)])], '', { 'aria-label': 'Built-in scenario', id: 'harness-builtin' });
      builtinSel.addEventListener('change', async () => {
        if (!builtinSel.value) return;
        const r = await send({ url: `${SERVER}${PREFIX}/scenarios/${builtinSel.value}`, timeout: 10000 });
        if (r.ok && r.json) {
          st.builtin = builtinSel.value;
          setScenario(r.json);
        }
      });
      let initial = null;
      if (st.scenarioText) {
        try { initial = JSON.parse(st.scenarioText); } catch { initial = null; }
      }
      if (!initial) {
        const r = await send({ url: `${SERVER}${PREFIX}/scenarios/${st.builtin || 'smoke'}`, timeout: 10000 });
        initial = (r.ok && r.json) || { name: 'smoke' };
      }
      form = scenarioForm(schema, normalise(initial));
      yamlArea = textarea({ rows: 12, id: 'harness-yaml', 'aria-label': 'Scenario as YAML' });
      yamlArea.value = toYaml(normalise(initial));
      const modes = h('div', { class: 'seg', role: 'group' });
      const formPane = h('div', null, form.el);
      const yamlPane = h('div', { hidden: true, class: 'stack-sm' }, yamlArea,
        h('div', { class: 'row' }, h('button', { class: 'btn sm', type: 'button', onclick: applyYaml }, 'Apply YAML to the form'), copyButton(() => yamlArea.value, { label: 'Copy YAML' })));
      const bForm = h('button', { type: 'button', 'aria-pressed': 'true', onclick: () => show('form') }, 'Form');
      const bYaml = h('button', { type: 'button', 'aria-pressed': 'false', onclick: () => show('yaml') }, 'YAML');
      modes.append(bForm, bYaml);
      function show(which) {
        const yaml = which === 'yaml';
        if (yaml) {
          try { yamlArea.value = toYaml(form.value()); } catch (e) { replace(problemsEl, notice('err', e.message)); return; }
        }
        formPane.hidden = yaml;
        yamlPane.hidden = !yaml;
        bForm.setAttribute('aria-pressed', String(!yaml));
        bYaml.setAttribute('aria-pressed', String(yaml));
      }
      replace(builderEl, h('div', { class: 'stack' },
        h('div', { class: 'row' }, builtinSel, modes, h('span', { class: 'grow' }),
          h('button', { class: 'btn', type: 'button', id: 'harness-validate', onclick: () => validate(false) }, 'Validate'),
          h('button', { class: 'btn primary', type: 'button', id: 'harness-run', onclick: () => validate(true) }, 'Run')),
        summaryEl, problemsEl, formPane, yamlPane));
    }

    // The builder edits the normalised form (targets and layers); the
    // shorthand fields of a scenario are folded into it.
    function normalise(sc) {
      const out = Object.assign({}, sc);
      if (out.target) { out.targets = [{ url: out.target }].concat(out.targets || []); delete out.target; }
      if (out.source || out.pattern || out.model || out.think_time) {
        const layer = { name: 'main', source: out.source || {}, pattern: out.pattern || { type: 'constant', rate: 10, for: '10s' } };
        if (out.model) layer.model = out.model;
        if (out.think_time) layer.think_time = out.think_time;
        out.layers = [layer].concat(out.layers || []);
        delete out.source; delete out.pattern; delete out.model; delete out.think_time;
      }
      return out;
    }

    function setScenario(sc) {
      const n = normalise(sc);
      form.set(n);
      yamlArea.value = toYaml(n);
      st.scenarioText = JSON.stringify(n);
      replace(problemsEl);
      replace(summaryEl);
    }

    async function applyYaml() {
      const r = await send({ method: 'POST', url: SERVER + PREFIX + '/validate', headers: { 'Content-Type': 'application/yaml' }, body: yamlArea.value, timeout: 10000 });
      if (!r.json) { replace(problemsEl, notice('err', r.error || `answered ${r.status}`)); return; }
      if (r.status === 400) { replace(problemsEl, notice('err', r.json.error)); return; }
      form.set(r.json.scenario);
      st.scenarioText = JSON.stringify(r.json.scenario);
      showValidation(r.json);
    }

    function showValidation(v) {
      const sum = v.summary || {};
      replace(summaryEl, `${sum.name || ''}: ${(sum.layers || []).length} layer(s), ${sum.targets} target(s), ${num(sum.duration_secs)} s, peak ${num(sum.peak_rate)} requests/s, about ${num(sum.expected_requests)} requests. Hash ${(v.hash || '').slice(0, 12)}.`);
      if (v.valid) replace(problemsEl, notice('ok', 'The scenario is valid for this node.'));
      else replace(problemsEl, notice('err', h('div', null, h('strong', null, `${v.problems.length} problem(s)`), h('ul', null, ...v.problems.map((p) => h('li', null, p))))));
      return v.valid;
    }

    async function validate(thenRun) {
      let value;
      try { value = form.value(); } catch (e) { replace(problemsEl, notice('err', e.message)); return; }
      st.scenarioText = JSON.stringify(value);
      const r = await send({ method: 'POST', url: SERVER + PREFIX + '/validate', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value), timeout: 10000 });
      if (!r.json) { replace(problemsEl, notice('err', r.error || `answered ${r.status}`)); return; }
      if (r.status === 400) { replace(problemsEl, notice('err', r.json.error)); return; }
      if (!showValidation(r.json) || !thenRun) return;
      const targets = (r.json.scenario.targets || []).map((t) => t.url).join(', ');
      if (!window.confirm(`Send load to ${targets} for ${num(r.json.summary.duration_secs)} s at a peak of ${num(r.json.summary.peak_rate)} requests/s?`)) return;
      const run = await send({ method: 'POST', url: SERVER + PREFIX + '/runs', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value), admin: true, timeout: 20000 });
      if (!run.json) { replace(problemsEl, notice('err', run.error || `answered ${run.status}`)); return; }
      if (run.status !== 202) {
        replace(problemsEl, notice('err', h('div', null, run.json.error, run.json.problems ? h('ul', null, ...run.json.problems.map((p) => h('li', null, p))) : null)));
        return;
      }
      st.runId = run.json.run_id;
      liveBuckets = [];
      replace(reportEl, h('p', { class: 'muted small' }, 'Running...'));
      toast('Run ' + st.runId + ' started');
      renderRun(run.json);
      startPolling();
      loadNode();
    }

    // ── Run ──
    function renderRun(r) {
      const last = r.last_second;
      if (last && (!liveBuckets.length || liveBuckets[liveBuckets.length - 1].t_secs !== last.t_secs)) liveBuckets.push(last);
      const active = r.state !== 'finished';
      replace(runEl, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('code', null, r.run_id), badge(r.state, active ? 'ok' : 'outline'), h('span', { class: 'muted small' }, r.scenario),
          r.saturated ? badge('client saturated', 'err') : null, h('span', { class: 'grow' }),
          active ? h('button', { class: 'btn sm', type: 'button', id: 'harness-stop', onclick: stopRun }, 'Stop') : null),
        h('div', { class: 'progress', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(Math.round((r.progress || 0) * 100)) }, h('div', { style: { width: Math.round((r.progress || 0) * 100) + '%' } })),
        h('div', { class: 'tiles' },
          tile('Elapsed', `${num(r.elapsed_secs, 1)} / ${num(r.planned_secs)} s`),
          tile('Sent', num(r.sent)),
          tile('Answered', num(r.completed)),
          tile('OK', num(r.ok), 'ok'),
          tile('Errors', num(r.errors), r.errors ? 'err-text' : ''),
          tile('Not sent', num(r.not_sent), r.not_sent ? 'err-text' : ''),
          tile('In flight', num(r.in_flight)),
          tile('p50 / p99', `${fmtMs(r.latency && r.latency.p50_ms)} / ${fmtMs(r.latency && r.latency.p99_ms)}`),
          tile('Last second', last ? `${num(last.rate)} /s, lag p99 ${fmtMs(last.lag_p99_ms)}` : '-')),
        timelineChart(liveBuckets),
        r.layers && r.layers.length > 1 ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Layer'), h('th', null, 'Model'), h('th', null, 'Target now'), h('th', null, 'Answered'), h('th', null, 'Errors'), h('th', null, 'p99'))),
          h('tbody', null, ...r.layers.map((l) => h('tr', null, h('td', null, l.name), h('td', null, l.model), h('td', null, l.target_now === null || l.target_now === undefined ? 'done' : num(l.target_now, 1)), h('td', null, num(l.completed)), h('td', null, num(l.errors)), h('td', null, fmtMs(l.p99_ms)))))) : null));
    }

    async function stopRun() {
      if (!st.runId) return;
      const r = await send({ method: 'POST', url: `${SERVER}${PREFIX}/runs/${st.runId}/stop`, admin: true, timeout: 10000 });
      if (r.ok) toast('Stopping the run');
      else toast((r.json && r.json.error) || r.error || `answered ${r.status}`, 'err');
    }

    function startPolling() {
      if (pollTimer) clearInterval(pollTimer);
      pollTimer = setInterval(poll, 1000);
      poll();
    }

    async function poll() {
      if (!alive || !st.runId) return;
      const r = await send({ url: `${SERVER}${PREFIX}/runs/${st.runId}`, timeout: 10000 });
      if (!alive) return;
      if (!r.ok || !r.json) {
        if (r.status === 404) { clearInterval(pollTimer); pollTimer = null; st.runId = null; }
        return;
      }
      renderRun(r.json);
      if (r.json.report_ready) {
        clearInterval(pollTimer);
        pollTimer = null;
        loadReport(r.json.run_id);
        loadNode();
      }
    }

    // ── Report ──
    async function loadReport(id) {
      const r = await send({ url: `${SERVER}${PREFIX}/runs/${id}/report`, timeout: 15000 });
      if (!alive || !r.ok || !r.json) return;
      const rep = r.json;
      const t = rep.totals || {};
      const lat = rep.latency || {};
      const sat = rep.saturation || {};
      liveBuckets = rep.timeline || [];
      replace(reportEl, h('div', { class: 'stack' },
        h('div', { class: 'row' }, badge(rep.passed ? 'PASS' : 'FAIL', rep.passed ? 'ok' : 'err'), badge(rep.state, 'outline'), h('span', { class: 'muted small' }, `exit code ${rep.exit_code}`),
          sat.saturated ? badge('client saturated: read the client, not the gateway', 'err') : badge('client not saturated', 'ok'), h('span', { class: 'grow' }),
          h('a', { class: 'btn sm', href: `${SERVER}${PREFIX}/runs/${id}/report`, target: '_blank', rel: 'noopener' }, 'JSON'),
          h('a', { class: 'btn sm', href: `${SERVER}${PREFIX}/runs/${id}/report?format=text`, target: '_blank', rel: 'noopener' }, 'Text'),
          h('a', { class: 'btn sm', href: `${SERVER}${PREFIX}/runs/${id}/report?format=junit`, target: '_blank', rel: 'noopener' }, 'JUnit')),
        h('div', { class: 'tiles' },
          tile('Requested', num(t.requested)), tile('Sent', num(t.sent)), tile('Answered', num(t.completed)), tile('OK', num(t.ok), 'ok'),
          tile('Unexpected status', num(t.unexpected_status), t.unexpected_status ? 'err-text' : ''), tile('Transport', num(t.transport), t.transport ? 'err-text' : ''),
          tile('Not sent', num(t.not_sent), t.not_sent ? 'err-text' : ''), tile('Achieved', `${num(rep.achieved_rate, 1)} /s of ${num(rep.requested_rate, 1)}`)),
        h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Latency'), h('th', null, 'p50'), h('th', null, 'p90'), h('th', null, 'p99'), h('th', null, 'p99.9'), h('th', null, 'max'), h('th', null, 'mean'))),
          h('tbody', null,
            h('tr', null, h('td', null, 'from the intended send'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(lat[k])))),
            h('tr', null, h('td', null, 'from the actual send'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs((rep.service_latency || {})[k])))))),
        rep.thresholds && rep.thresholds.length ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Threshold'), h('th', null, 'Limit'), h('th', null, 'Measured'), h('th', null, 'Result'))),
          h('tbody', null, ...rep.thresholds.map((th) => h('tr', null, h('td', null, th.name), h('td', { class: 'mono' }, String(th.limit)), h('td', { class: 'mono' }, String(th.measured)), h('td', null, badge(th.pass ? 'pass' : 'fail', th.pass ? 'ok' : 'err')))))) : h('p', { class: 'muted small' }, 'No thresholds in the scenario.'),
        timelineChart(liveBuckets),
        (rep.layers || []).length > 1 || (rep.targets_breakdown || []).length > 1 ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Breakdown'), h('th', null, 'Sent'), h('th', null, 'OK'), h('th', null, 'Errors'), h('th', null, 'p50'), h('th', null, 'p99'))),
          h('tbody', null, ...[...(rep.layers || []).map((b) => ['layer', b]), ...(rep.targets_breakdown || []).map((b) => ['target', b])].map(([kind, b]) => h('tr', null, h('td', null, `${kind} ${b.name}`), h('td', null, num(b.counts.sent)), h('td', null, num(b.counts.ok)), h('td', null, num(b.counts.unexpected_status + b.counts.transport)), h('td', null, fmtMs(b.latency.p50_ms)), h('td', null, fmtMs(b.latency.p99_ms)))))) : null,
        sat.reasons && sat.reasons.length ? notice('warn', sat.reasons.join(' ')) : null,
        h('details', { class: 'disclosure' }, h('summary', null, `Samples (${(rep.samples || []).length}) with their X-Request-Id`),
          h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
            h('thead', null, h('tr', null, h('th', null, 'Kind'), h('th', null, 'Request id'), h('th', null, 'Request'), h('th', null, 'Status'), h('th', null, 'Latency'))),
            h('tbody', null, ...(rep.samples || []).slice(0, 50).map((x) => h('tr', null, h('td', null, x.kind), h('td', { class: 'mono' }, x.request_id), h('td', { class: 'mono' }, `${x.method} ${x.path}`), h('td', null, x.status !== null && x.status !== undefined ? String(x.status) : (x.error || '-')), h('td', null, fmtMs(x.latency_ms)))))))),
        h('details', { class: 'disclosure' }, h('summary', null, 'Full report (JSON)'), jsonView(rep))));
    }

    loadNode().then((n) => { if (n && n.enabled) loadBuilder(); else replace(builderEl, h('p', { class: 'muted small' }, 'Turn harness mode on to build and run scenarios.')); });
    if (st.runId) startPolling();
    const refresh = setInterval(loadNode, 15000);

    return () => {
      alive = false;
      if (pollTimer) clearInterval(pollTimer);
      clearInterval(refresh);
      clear(root);
    };
  },
};
