// Harness: this Rustybin as a load client. Node status and caps, the
// cluster (members, leader, upstreams; create one on a coordinator), a
// scenario builder generated from the server's JSON Schema (with a YAML view
// and the built-in scenarios), validation, a live run view with a per-second
// chart, the kill switch, and the final report with its thresholds. A run
// goes to this node or to a cluster it leads. Phase 4: a preview chart that
// redraws as the scenario changes, saved scenarios, live controls on a
// running run (rate multiplier, pause, hold, skip stage, set), and the run's
// event feed (read with send({stream: true}), polling when it is not there).
import { h, s, replace, clear, card, kvgrid, notice, badge, spinner, toast, select, textarea, input, fmtMs, copyButton } from '../lib/dom.js';
import { jsonView } from '../lib/format.js';
import { SERVER, send, getJson, sseParser, readText } from '../lib/http.js';
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

// "step 1, part 2": a pattern stage counted from 1 and, for stepped and
// periodic stages, its part (a plateau, half a pulse cycle or a burst).
const PARTED = ['step', 'breakpoint', 'pulse', 'burst'];
function stageLabel(type, index, part) {
  return `${type} ${index + 1}${PARTED.includes(type) ? `, part ${part + 1}` : ''}`;
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
    svg.appendChild(s('rect', { x: x + 1, y: H - pad - hr, width: Math.max(1, bw - 2), height: hr, fill: b.saturated ? 'var(--status-critical)' : b.errors ? 'var(--status-warning)' : 'var(--status-good)' }, s('title', null, `${b.t_secs}s: ${b.completed} answered (${b.upstream || 0} upstream, ${b.gateway || 0} gateway), ${b.errors} errors, ${b.not_sent} not sent, p99 ${fmtMs(b.p99_ms)}, overhead p99 ${fmtMs(b.overhead_p99_ms)}`)));
    const hq = ((b.requested || 0) / maxRate) * (H - pad * 2);
    svg.appendChild(s('line', { x1: x + 1, y1: H - pad - hq, x2: x + bw - 1, y2: H - pad - hq, stroke: 'var(--text-3)', 'stroke-dasharray': '2 2' }));
  });
  const pts = buckets.map((b, i) => `${pad + i * bw + bw / 2},${H - pad - ((b.p99_ms || 0) / maxP99) * (H - pad * 2)}`).join(' ');
  svg.appendChild(s('polyline', { points: pts, fill: 'none', stroke: 'var(--info)', 'stroke-width': 2 }));
  svg.appendChild(s('text', { x: pad, y: 12, fill: 'var(--text-3)', 'font-size': 10 }, `answers/s (max ${num(maxRate)}), dashed: requested`));
  svg.appendChild(s('text', { x: W - pad, y: 12, fill: 'var(--info)', 'font-size': 10, 'text-anchor': 'end' }, `p99 latency (max ${fmtMs(maxP99)})`));
  return svg;
}

// The preview: expected requests per second, the total and each layer.
function previewChart(p) {
  const W = 640, H = 140, pad = 28;
  const total = p.total || [];
  if (!total.length) return h('p', { class: 'muted small' }, 'Nothing to preview.');
  const layers = p.layers || [];
  const max = Math.max(1, ...total, ...layers.flatMap((l) => l.series || []));
  const n = total.length;
  const x = (i) => pad + (i + 0.5) * ((W - pad * 2) / n);
  const y = (v) => H - pad - (v / max) * (H - pad * 2);
  const svg = s('svg', { class: 'chart', viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': 'Expected requests per second' });
  svg.appendChild(s('line', { x1: pad, y1: H - pad, x2: W - pad, y2: H - pad, stroke: 'var(--border)' }));
  const line = (series, color, width) => s('polyline', { points: series.map((v, i) => `${x(i)},${y(v)}`).join(' '), fill: 'none', stroke: color, 'stroke-width': width });
  const colors = ['var(--info)', 'var(--status-warning)', 'var(--status-good)', 'var(--text-3)'];
  if (layers.length > 1) layers.forEach((l, i) => svg.appendChild(line(l.series || [], colors[i % colors.length], 1)));
  svg.appendChild(line(total, 'var(--text)', 2));
  svg.appendChild(s('text', { x: pad, y: 12, fill: 'var(--text-3)', 'font-size': 10 }, `expected requests per second (max ${num(max)}), ${num(p.duration_secs)} s${p.step_secs > 1 ? `, ${p.step_secs} s per point` : ''}`));
  return h('div', { class: 'stack-sm' }, svg,
    layers.length > 1 ? h('p', { class: 'muted small' }, layers.map((l) => `${l.name}: peak ${num(l.peak)}`).join(', ')) : null);
}

export default {
  id: 'harness',
  mount(root) {
    let alive = true;
    let pollTimer = null;
    let liveBuckets = [];
    const st = settings.recall('harness.state', { runId: null, scenarioText: '', builtin: 'smoke', cluster: null });
    // Where runs go: this node, or a cluster it leads.
    const runBase = () => (st.cluster ? `${PREFIX}/clusters/${encodeURIComponent(st.cluster)}` : PREFIX);
    settings.remember('harness.state', st);

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Harness'),
        h('p', null, 'Rustybin as the load client: send a shaped load from this node to a gateway (or back to itself), with every target allowlisted and the client watching itself for saturation, so a slow client is never read as a slow gateway.'))));

    const nodeEl = h('div', null, spinner('Reading the node status'));
    const builderEl = h('div', null, spinner('Loading the scenario schema'));
    const runEl = h('div', null, h('p', { class: 'muted small' }, 'No run yet. Validate a scenario and start it.'));
    // Live controls of the running run, and its log (changes, findings).
    const controlsEl = h('div', { id: 'harness-controls' });
    const logEl = h('div', { class: 'stack-sm small', id: 'harness-log' });
    const reportEl = h('div', null, h('p', { class: 'muted small' }, 'The report appears when a run finishes.'));
    const killMsg = h('div');
    const killBtn = h('button', { class: 'btn sm', type: 'button', id: 'harness-kill', onclick: killSwitch }, 'Kill switch: stop all load');
    const clusterEl = h('div');
    const clusterCard = card('Cluster', { hint: 'GET /_rustybin/harness/clusters/{id}' }, clusterEl);
    clusterCard.hidden = true;

    root.appendChild(h('div', { class: 'grid grid-2' },
      card('Node', { hint: 'GET /_rustybin/harness', actions: h('div', { class: 'row' }, killBtn) }, nodeEl, killMsg),
      card('Run', { hint: 'live, one second at a time' }, runEl, controlsEl, logEl)));
    root.appendChild(clusterCard);
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
      if (n.active_run && !st.runId && !st.cluster) {
        st.runId = n.active_run.run_id;
        startFeed();
      }
      renderCluster(n);
      return n;
    }

    // ── Cluster ──
    let joinToken = null;
    // The leader changes of a cluster, oldest first.
    function historyLine(history) {
      if (!history || !history.length) return null;
      return h('p', { class: 'small' }, 'Leaders: ', history.map((x) => `term ${x.term} ${x.node_id} (${x.reason})`).join(', then '));
    }
    // Who leads next: standbys first, then by work score.
    function successionLine(succession) {
      if (!succession || !succession.length) return h('p', { class: 'muted small' }, 'No other node may lead: set RUSTYBIN_HARNESS_CONTROL_URL on at least two more nodes for failover.');
      return h('p', { class: 'small' }, 'Next in line: ', succession.map((x, i) => `${i + 1}. ${x.node_id}${x.prefer ? ' (standby)' : ''}, work ${num(x.work_score, 1)}`).join('; '));
    }
    async function renderCluster(n) {
      const leads = n.clusters || [];
      const coordinator = (n.roles || []).includes('coordinator');
      clusterCard.hidden = !coordinator && !leads.length && !n.cluster;
      if (clusterCard.hidden) return;
      if (st.cluster && !leads.some((c) => c.cluster_id === st.cluster)) st.cluster = null;
      // Without the client role, runs can only go to a cluster.
      if (!st.cluster && leads.length && !(n.roles || []).includes('client')) st.cluster = leads[0].cluster_id;
      const parts = [];
      if (joinToken) {
        parts.push(notice('ok', h('div', { class: 'stack-sm' },
          h('div', null, `Cluster ${joinToken.id} created. Nodes join with RUSTYBIN_HARNESS_JOIN=${joinToken.url} and this join token (shown once):`),
          h('div', { class: 'row' }, h('code', null, joinToken.token), copyButton(() => joinToken.token, { label: 'Copy token' })))));
      }
      for (const c of leads) {
        const r = await send({ url: `${SERVER}${PREFIX}/clusters/${encodeURIComponent(c.cluster_id)}`, timeout: 10000 });
        if (!alive) return;
        if (!r.ok || !r.json) { parts.push(notice('err', r.error || `answered ${r.status}`)); continue; }
        const d = r.json;
        const ups = await send({ url: `${SERVER}${PREFIX}/clusters/${encodeURIComponent(c.cluster_id)}/upstreams`, timeout: 10000 });
        const upstreams = (ups.ok && Array.isArray(ups.json)) ? ups.json : [];
        parts.push(h('div', { class: 'stack-sm' },
          h('div', { class: 'row' }, h('strong', null, d.cluster_id), badge(`leader ${d.leader.node_id}, term ${d.term}`, 'violet'),
            d.quorum === false ? badge('no majority of the eligible nodes: leases not renewed', 'err') : null,
            badge(`${d.alive.clients} client(s), ${d.alive.servers} server(s) alive`, 'outline'),
            d.current_run ? badge(`run ${d.current_run.run_id} ${d.current_run.state}`, d.current_run.state === 'finished' ? 'outline' : 'ok') : null,
            h('span', { class: 'grow' }),
            h('label', { class: 'row small' }, h('input', { type: 'radio', name: 'harness-run-on', checked: st.cluster === d.cluster_id, onchange: () => { st.cluster = d.cluster_id; st.runId = null; toast(`Runs go to cluster ${d.cluster_id}`); } }), 'Run on this cluster')),
          h('p', { class: 'muted small' }, `Lease ${num(d.lease_ms)} ms: a client stops sending when no leader has answered for that long. Election timeout ${num(d.election_timeout_ms)} ms. Failover: ${d.failover} (${num(d.eligible)} eligible).`),
          historyLine(d.history),
          h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
            h('thead', null, h('tr', null, h('th', null, 'Node'), h('th', null, 'Roles'), h('th', null, 'Health'), h('th', null, 'Next'), h('th', null, 'Work'), h('th', null, 'Capacity'), h('th', null, 'Clock offset'), h('th', null, 'In flight'), h('th', null, 'Advertise'))),
            h('tbody', null, ...(d.members || []).map((m) => h('tr', null,
              h('td', { class: 'mono' }, m.node_id, m.node_id === d.leader.node_id ? ' (leader)' : ''),
              h('td', null, (m.roles || []).join(', ')),
              h('td', null, badge(m.health, m.health === 'healthy' ? 'ok' : m.health === 'late' ? 'warn' : 'err')),
              h('td', null, m.leader ? 'leads' : m.succession ? `#${m.succession}${m.leader_policy === 'prefer' ? ' (standby)' : ''}` : m.eligible ? '-' : 'never'),
              h('td', { class: 'mono' }, m.work_score === undefined ? '-' : num(m.work_score, 1)),
              h('td', null, `${num(m.capacity.max_rate)} /s`),
              h('td', { class: 'mono' }, m.clock_offset_ms === null || m.clock_offset_ms === undefined ? '-' : `${num(m.clock_offset_ms, 2)} ms (+/- ${num(m.clock_error_ms, 2)})`),
              h('td', null, num(m.in_flight)),
              h('td', { class: 'mono' }, m.advertise || '-')))))),
          upstreams.length ? h('div', { class: 'row small' }, h('span', null, `Upstreams for the gateway: ${upstreams.join(', ')}`), copyButton(() => upstreams.join('\n'), { label: 'Copy' })) : h('p', { class: 'muted small' }, 'No server node with an advertised URL yet.')));
      }
      if (leads.length && (n.roles || []).includes('client')) {
        parts.push(h('label', { class: 'row small' }, h('input', { type: 'radio', name: 'harness-run-on', checked: !st.cluster, onchange: () => { st.cluster = null; st.runId = null; toast('Runs go to this node'); } }), 'Run on this node only'));
      }
      if (n.cluster && !leads.some((c) => c.cluster_id === n.cluster.cluster_id)) {
        const m = n.cluster;
        // Every member answers who leads, as its snapshot says.
        const lr = await send({ url: `${SERVER}${PREFIX}/clusters/${encodeURIComponent(m.cluster_id)}/leader`, timeout: 10000 });
        if (!alive) return;
        const lv = (lr.ok && lr.json) || {};
        parts.push(h('div', { class: 'row' }, h('strong', null, m.cluster_id), m.leader && m.leader.node_id ? badge(`leader ${m.leader.node_id}, term ${m.leader.term}`, 'violet') : badge('not joined yet', 'outline'),
          lv.failover ? badge(`failover ${lv.failover.split(':')[0]}`, lv.failover === 'available' ? 'ok' : 'outline') : null));
        parts.push(kvgrid([
          ['Member of', h('code', null, m.cluster_id)],
          ['Leader', m.leader && m.leader.node_id ? `${m.leader.node_id}, term ${m.leader.term}` : 'not joined yet'],
          ['Leader URL', m.url || 'this node (in process)'],
          ['Last answer', m.last_answer_ms === null || m.last_answer_ms === undefined ? '-' : `${num(m.last_answer_ms)} ms ago (lease ${num(m.lease_ms)} ms)`],
          ['Clock offset', m.clock_offset_ms === null || m.clock_offset_ms === undefined ? '-' : `${num(m.clock_offset_ms, 2)} ms (+/- ${num(m.clock_error_ms, 2)})`],
          ['Last error', m.last_error || 'none'],
        ]));
        if (lv.succession) parts.push(successionLine(lv.succession));
        parts.push(historyLine(lv.history));
      }
      if (coordinator && !leads.length) {
        const idIn = input({ placeholder: 'cluster id (optional)', 'aria-label': 'Cluster id', id: 'harness-cluster-id' });
        const minC = input({ type: 'number', min: '0', value: '1', 'aria-label': 'Client nodes to wait for', style: { width: '6em' } });
        const minS = input({ type: 'number', min: '0', value: '0', 'aria-label': 'Server nodes to wait for', style: { width: '6em' } });
        const msg = h('div');
        const create = async () => {
          const body = { min_nodes: { client: Number(minC.value) || 0, server: Number(minS.value) || 0 } };
          if (idIn.value.trim()) body.cluster_id = idIn.value.trim();
          const r = await send({ method: 'POST', url: SERVER + PREFIX + '/clusters', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), admin: true, timeout: 10000 });
          if (r.status !== 201 || !r.json) { replace(msg, notice('err', (r.json && r.json.error) || r.error || `answered ${r.status}`)); return; }
          joinToken = { id: r.json.cluster_id, token: r.json.join_token, url: location.origin.replace(/\/$/, '') + SERVER + PREFIX + '/clusters/' + r.json.cluster_id };
          st.cluster = r.json.cluster_id;
          loadNode();
        };
        parts.push(h('div', { class: 'stack-sm' },
          h('p', { class: 'small' }, 'This node is a coordinator: create a cluster, then start client and server nodes with its join URL and token. Its client nodes share the rate by capacity and its server nodes are the gateway\'s upstreams.'),
          h('div', { class: 'row' }, idIn, h('span', { class: 'small' }, 'wait for'), minC, h('span', { class: 'small' }, 'client(s) and'), minS, h('span', { class: 'small' }, 'server(s)'),
            h('button', { class: 'btn sm', type: 'button', id: 'harness-create-cluster', onclick: create }, 'Create cluster')), msg));
      }
      replace(clusterEl, h('div', { class: 'stack' }, ...parts));
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
      const savedList = (lr.json && lr.json.saved) || [];
      builtinSel = select([['', 'Scenarios...'], ...builtins.map((b) => [b.name, `${b.name}: ${b.description || ''}`.slice(0, 90)]), ...savedList.map((b) => [b.name, `saved: ${b.name}`])], '', { 'aria-label': 'Built-in or saved scenario', id: 'harness-builtin' });
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
      // The preview redraws as the scenario changes.
      form.el.addEventListener('input', schedulePreview);
      form.el.addEventListener('change', schedulePreview);
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
          h('button', { class: 'btn', type: 'button', id: 'harness-save', onclick: saveAs }, 'Save as'),
          h('button', { class: 'btn', type: 'button', id: 'harness-validate', onclick: () => validate(false) }, 'Validate'),
          h('button', { class: 'btn primary', type: 'button', id: 'harness-run', onclick: () => validate(true) }, 'Run')),
        summaryEl, problemsEl, previewEl, formPane, yamlPane));
      schedulePreview();
    }

    // ── Preview and saved scenarios ──
    const previewEl = h('div', { id: 'harness-preview' });
    let previewTimer = null;
    function schedulePreview() {
      clearTimeout(previewTimer);
      previewTimer = setTimeout(runPreview, 500);
    }
    async function runPreview() {
      if (!form || !alive) return;
      let value;
      try { value = form.value(); } catch { return; }
      const r = await send({ method: 'POST', url: SERVER + PREFIX + '/preview', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value), timeout: 10000 });
      if (!alive || !r.ok || !r.json) return;
      replace(previewEl, previewChart(r.json));
    }

    async function saveAs() {
      let value;
      try { value = form.value(); } catch (e) { replace(problemsEl, notice('err', e.message)); return; }
      const name = window.prompt('Save the scenario as (letters, digits, . _ -):', (value.name || 'scenario').replace(/[^A-Za-z0-9._-]/g, '-'));
      if (!name) return;
      const r = await send({ method: 'PUT', url: `${SERVER}${PREFIX}/scenarios/${encodeURIComponent(name)}`, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value), admin: true, timeout: 10000 });
      if (r.ok) {
        toast(`Saved as ${name}`);
        if (builtinSel && ![...builtinSel.options].some((o) => o.value === name)) builtinSel.append(h('option', { value: name }, `saved: ${name}`));
      } else {
        replace(problemsEl, notice('err', h('div', null, (r.json && r.json.error) || r.error || `answered ${r.status}`, r.json && r.json.problems ? h('ul', null, ...r.json.problems.map((p) => h('li', null, p))) : null)));
      }
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
      schedulePreview();
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
      schedulePreview();
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
      const where = st.cluster ? ` from the client nodes of cluster ${st.cluster}` : '';
      if (!window.confirm(`Send load to ${targets}${where} for ${num(r.json.summary.duration_secs)} s at a peak of ${num(r.json.summary.peak_rate)} requests/s?`)) return;
      const showError = (res) => replace(problemsEl, notice('err', h('div', null, res.json.error, res.json.problems ? h('ul', null, ...res.json.problems.map((p) => h('li', null, p))) : null)));
      let run = await send({ method: 'POST', url: SERVER + runBase() + '/runs', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value), admin: true, timeout: 20000 });
      if (!run.json) { replace(problemsEl, notice('err', run.error || `answered ${run.status}`)); return; }
      if (st.cluster) {
        // A cluster run is submitted, then started once the quorum is there.
        if (run.status !== 201) { showError(run); return; }
        run = await send({ method: 'POST', url: `${SERVER}${runBase()}/runs/${run.json.run_id}/start`, admin: true, timeout: 20000 });
        if (!run.json) { replace(problemsEl, notice('err', run.error || `answered ${run.status}`)); return; }
      }
      if (run.status !== 202) {
        showError(run);
        return;
      }
      st.runId = run.json.run_id;
      liveBuckets = [];
      replace(reportEl, h('p', { class: 'muted small' }, 'Running...'));
      toast('Run ' + st.runId + ' started');
      renderRun(run.json);
      replace(logEl);
      startFeed();
      loadNode();
    }

    // ── Run ──
    function renderRun(r) {
      renderControls(r);
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
          tile('Upstream / gateway', `${num(r.upstream)} / ${num(r.gateway)}`, r.gateway ? 'err-text' : ''),
          tile('In flight', num(r.in_flight)),
          tile('p50 / p99', `${fmtMs(r.latency && r.latency.p50_ms)} / ${fmtMs(r.latency && r.latency.p99_ms)}`),
          tile('Overhead p99', r.overhead && r.overhead.count ? fmtMs(r.overhead.p99_ms) : '-'),
          tile('Last second', last ? `${num(last.rate)} /s, lag p99 ${fmtMs(last.lag_p99_ms)}` : '-')),
        timelineChart(liveBuckets),
        r.cluster && r.nodes ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Node'), h('th', null, 'Role'), h('th', null, 'Share'), h('th', null, 'State'), h('th', null, 'Sent'), h('th', null, 'Answered / arrivals'), h('th', null, 'Start lag'))),
          h('tbody', null, ...r.nodes.map((x) => h('tr', null, h('td', { class: 'mono' }, x.node_id), h('td', null, x.role),
            h('td', null, x.role === 'client' ? `${num(x.share * 100, 1)}%` : '-'),
            h('td', null, x.role === 'client' ? badge(x.state, x.error ? 'err' : 'outline') : badge(x.alive ? 'alive' : 'gone', x.alive ? 'ok' : 'err')),
            h('td', null, x.role === 'client' ? num(x.sent) : '-'),
            h('td', null, num(x.role === 'client' ? x.completed : x.arrivals)),
            h('td', null, x.start_lag_ms === null || x.start_lag_ms === undefined ? '-' : fmtMs(x.start_lag_ms)))))) : null,
        r.layers && r.layers.length > 1 ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Layer'), h('th', null, 'Model'), h('th', null, 'Target now'), h('th', null, 'Answered'), h('th', null, 'Errors'), h('th', null, 'p99'))),
          h('tbody', null, ...r.layers.map((l) => h('tr', null, h('td', null, l.name), h('td', null, l.model), h('td', null, l.target_now === null || l.target_now === undefined ? 'done' : num(l.target_now, 1)), h('td', null, num(l.completed)), h('td', null, num(l.errors)), h('td', null, fmtMs(l.p99_ms)))))) : null));
    }

    async function stopRun() {
      if (!st.runId) return;
      const r = await send({ method: 'POST', url: `${SERVER}${runBase()}/runs/${st.runId}/stop`, admin: true, timeout: 10000 });
      if (r.ok) toast('Stopping the run');
      else toast((r.json && r.json.error) || r.error || `answered ${r.status}`, 'err');
    }

    function startPolling() {
      if (pollTimer) clearInterval(pollTimer);
      pollTimer = setInterval(poll, 1000);
      poll();
    }

    // The run's feed: a snapshot every second, the changes and findings,
    // `finished` at the end. Polling when the feed is not there.
    let feedCtrl = null;
    function stopFeed() {
      if (feedCtrl) feedCtrl.abort();
      feedCtrl = null;
    }
    function addLog(text, kind) {
      const line = h('div', { class: kind === 'warn' ? 'warn-text' : kind === 'ok' ? 'ok' : 'muted' }, text);
      logEl.prepend(line);
      while (logEl.children.length > 30) logEl.lastChild.remove();
    }
    async function startFeed() {
      stopFeed();
      if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
      const id = st.runId;
      if (!id || !alive) return;
      const ctrl = new AbortController();
      feedCtrl = ctrl;
      const r = await send({ url: `${SERVER}${runBase()}/runs/${id}/events`, stream: true, timeout: 15000, signal: ctrl.signal });
      if (r.error || !r.response || !r.contentType.includes('event-stream')) {
        if (!ctrl.signal.aborted) startPolling();
        return;
      }
      let finished = false;
      try {
        await readText(r.response, sseParser((ev) => {
          let data = null;
          try { data = JSON.parse(ev.data); } catch { data = null; }
          if (!data) return;
          if (ev.event === 'snapshot') renderRun(data);
          else if (ev.event === 'change') addLog(`${num(data.at_secs, 1)} s: ${data.what}`);
          else if (ev.event === 'finding') addLog(data.summary, 'ok');
          else if (ev.event === 'scale_request') addLog(`scale request: ${data.needed_client_nodes} client node(s) needed, ${data.client_nodes} here (${data.reason})`, 'warn');
          else if (ev.event === 'leader_changed') addLog(`the leader changed${data.leader ? `: ${data.leader.node_id}, term ${data.leader.term}` : ''}; follow the run there`, 'warn');
          else if (ev.event === 'finished') {
            finished = true;
            loadReport(id);
            loadNode();
          }
        }), ctrl.signal);
      } catch { /* the stream broke off */ }
      if (feedCtrl === ctrl) feedCtrl = null;
      // A feed that ended early: poll instead.
      if (!finished && alive && !ctrl.signal.aborted && st.runId === id) startPolling();
    }

    // Live changes (PATCH): applied on the next pass, on every node.
    let controlsKey = '';
    async function change(body) {
      if (!st.runId) return;
      const r = await send({ method: 'PATCH', url: `${SERVER}${runBase()}/runs/${st.runId}`, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), admin: true, timeout: 10000 });
      if (r.ok) {
        toast(`Applied: ${r.json.applied}`);
        controlsKey = '';
      } else {
        const problems = (r.json && r.json.problems) || [];
        toast(((r.json && r.json.error) || r.error || `answered ${r.status}`) + (problems.length ? `: ${problems.join('; ')}` : ''), 'err');
      }
    }
    // The panel is rebuilt only when the controls change (so the inputs keep
    // what is being typed); the level and stage cells follow every snapshot.
    let controlCells = new Map();
    function fillControlCells(layers) {
      for (const l of layers) {
        const c = controlCells.get(l.name);
        if (!c) continue;
        c.now.textContent = l.target_now === null || l.target_now === undefined ? '-' : num(l.target_now, 1);
        replace(c.stage, l.stage ? stageLabel(l.stage.type, l.stage.index, l.stage.part) : '-', l.held ? badge('held', 'warn') : null, l.paused ? badge('paused', 'warn') : null);
      }
    }
    function renderControls(r) {
      const active = !['finished', 'collecting', 'created'].includes(r.state);
      const layers = r.layers || [];
      const key = `${r.run_id}|${active}|${r.controls_version}|${layers.map((l) => `${l.name}:${l.paused}:${l.held}:${l.removed}:${l.multiplier}`).join(',')}`;
      if (key === controlsKey) { fillControlCells(layers); return; }
      controlsKey = key;
      controlCells = new Map();
      if (!active) { replace(controlsEl); return; }
      const mult = r.multiplier ?? 1;
      const slider = h('input', { type: 'range', min: '0', max: '3', step: '0.1', value: String(mult), 'aria-label': 'Rate multiplier of the run', id: 'harness-mult' });
      const shown = h('span', { class: 'mono small' }, `x${mult}`);
      slider.addEventListener('input', () => { shown.textContent = 'x' + slider.value; });
      slider.addEventListener('change', () => change({ rate_multiplier: Number(slider.value) }));
      const setPath = input({ placeholder: `${(layers[0] && layers[0].name) || 'main'}.pattern.rate`, 'aria-label': 'Pattern parameter to set', id: 'harness-set-path', style: { maxWidth: '16em' } });
      const setValue = input({ placeholder: 'value', 'aria-label': 'New value', id: 'harness-set-value', style: { maxWidth: '8em' } });
      const applySet = () => {
        const path = setPath.value.trim();
        if (!path) return;
        let value = setValue.value.trim();
        try { value = JSON.parse(value); } catch { /* a string such as 30s */ }
        change({ set: { [path]: value } });
      };
      replace(controlsEl, h('div', { class: 'stack-sm' },
        h('h3', null, 'Live controls'),
        h('div', { class: 'row' }, h('span', { class: 'small' }, 'Rate'), slider, shown,
          h('button', { class: 'btn sm', type: 'button', onclick: () => change(r.paused ? { resume: true } : { pause: true }) }, r.paused ? 'Resume' : 'Pause'),
          h('button', { class: 'btn sm', type: 'button', onclick: () => change({ hold: !layers.every((l) => l.held) }) }, layers.length && layers.every((l) => l.held) ? 'Release' : 'Hold'),
          h('button', { class: 'btn sm', type: 'button', onclick: () => change({ skip_stage: true }) }, 'Skip stage')),
        layers.length > 1 || (layers[0] && (layers[0].held || layers[0].paused || layers[0].multiplier !== 1)) ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Layer'), h('th', null, 'Now'), h('th', null, 'Stage'), h('th', null, 'x'), h('th', null, ''))),
          h('tbody', null, ...layers.map((l) => {
            const now = h('td', { class: 'mono' });
            const stage = h('td');
            controlCells.set(l.name, { now, stage });
            return h('tr', null,
              h('td', null, l.name, l.removed ? badge('removed', 'outline') : null),
              now, stage,
              h('td', { class: 'mono' }, `x${l.multiplier ?? 1}`),
              h('td', null, l.removed ? null : h('div', { class: 'row' },
                h('button', { class: 'btn sm', type: 'button', onclick: () => change(l.paused ? { layer: l.name, resume: true } : { layer: l.name, pause: true }) }, l.paused ? 'Resume' : 'Pause'),
                h('button', { class: 'btn sm', type: 'button', onclick: () => change({ layer: l.name, hold: !l.held }) }, l.held ? 'Release' : 'Hold'),
                h('button', { class: 'btn sm', type: 'button', onclick: () => change({ layer: l.name, skip_stage: true }) }, 'Skip'))));
          }))) : null,
        h('div', { class: 'row' }, h('span', { class: 'small' }, 'Set'), setPath, setValue,
          h('button', { class: 'btn sm', type: 'button', onclick: applySet }, 'Apply'))));
      fillControlCells(layers);
    }

    async function poll() {
      if (!alive || !st.runId) return;
      const r = await send({ url: `${SERVER}${runBase()}/runs/${st.runId}`, timeout: 10000 });
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
      const base = runBase();
      const r = await send({ url: `${SERVER}${base}/runs/${id}/report`, timeout: 15000 });
      if (!alive || !r.ok || !r.json) return;
      const rep = r.json;
      const t = rep.totals || {};
      const lat = rep.latency || {};
      const sat = rep.saturation || {};
      liveBuckets = rep.timeline || [];
      replace(reportEl, h('div', { class: 'stack' },
        h('div', { class: 'row' }, badge(rep.passed ? 'PASS' : 'FAIL', rep.passed ? 'ok' : 'err'), badge(rep.state, 'outline'), h('span', { class: 'muted small' }, `exit code ${rep.exit_code}`),
          sat.saturated ? badge('client saturated: read the client, not the gateway', 'err') : badge('client not saturated', 'ok'), h('span', { class: 'grow' }),
          h('a', { class: 'btn sm', href: `${SERVER}${base}/runs/${id}/report`, target: '_blank', rel: 'noopener' }, 'JSON'),
          h('a', { class: 'btn sm', href: `${SERVER}${base}/runs/${id}/report?format=text`, target: '_blank', rel: 'noopener' }, 'Text'),
          h('a', { class: 'btn sm', href: `${SERVER}${base}/runs/${id}/report?format=junit`, target: '_blank', rel: 'noopener' }, 'JUnit')),
        h('div', { class: 'tiles' },
          tile('Requested', num(t.requested)), tile('Sent', num(t.sent)), tile('Answered', num(t.completed)), tile('OK', num(t.ok), 'ok'),
          tile('Unexpected status', num(t.unexpected_status), t.unexpected_status ? 'err-text' : ''), tile('Transport', num(t.transport), t.transport ? 'err-text' : ''),
          tile('Not sent', num(t.not_sent), t.not_sent ? 'err-text' : ''), tile('Achieved', `${num(rep.achieved_rate, 1)} /s of ${num(rep.requested_rate, 1)}`),
          tile('Upstream', num(t.upstream), 'ok'), tile('Gateway', num(t.gateway), t.gateway ? 'err-text' : ''),
          tile('Lost', num(t.lost), t.lost ? 'err-text' : ''), tile('Overhead p50 / p99', rep.overhead && rep.overhead.count ? `${fmtMs(rep.overhead.p50_ms)} / ${fmtMs(rep.overhead.p99_ms)}` : 'no tagged answers')),
        t.gateway_statuses && Object.keys(t.gateway_statuses).length ? h('p', { class: 'small' }, 'Gateway answers by status: ', Object.entries(t.gateway_statuses).map(([k, v]) => `${k}: ${num(v)}`).join(', ')) : null,
        h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Latency'), h('th', null, 'p50'), h('th', null, 'p90'), h('th', null, 'p99'), h('th', null, 'p99.9'), h('th', null, 'max'), h('th', null, 'mean'))),
          h('tbody', null,
            h('tr', null, h('td', null, 'from the intended send'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(lat[k])))),
            h('tr', null, h('td', null, 'from the actual send'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs((rep.service_latency || {})[k])))),
            rep.upstream_latency && rep.upstream_latency.count ? h('tr', null, h('td', null, 'upstream processing (Server-Timing)'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(rep.upstream_latency[k])))) : null,
            rep.overhead && rep.overhead.count ? h('tr', null, h('td', null, 'gateway overhead (total minus upstream)'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(rep.overhead[k])))) : null,
            rep.request_leg && rep.request_leg.count ? h('tr', null, h('td', null, 'request leg (one clock)'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(rep.request_leg[k])))) : null,
            rep.response_leg && rep.response_leg.count ? h('tr', null, h('td', null, 'response leg (one clock)'), ...['p50_ms', 'p90_ms', 'p99_ms', 'p999_ms', 'max_ms', 'mean_ms'].map((k) => h('td', { class: 'mono' }, fmtMs(rep.response_leg[k])))) : null)),
        rep.server ? h('div', { class: 'stack-sm' },
          h('h3', null, 'Server side'),
          kvgrid([
            ['Arrivals', `${num(rep.server.arrivals)} of ${num(t.sent)} sent`],
            ['Processing', `p50 ${fmtMs(rep.server.processing.p50_ms)}, p99 ${fmtMs(rep.server.processing.p99_ms)}`],
            ['Captured in the inspector', num(rep.server.captured)],
            ['Headers added by the gateway', Object.keys(rep.server.changes.headers_added).length ? Object.entries(rep.server.changes.headers_added).map(([k, v]) => `${k} (${num(v)})`).join(', ') : 'none'],
            ['Headers missing', Object.keys(rep.server.changes.headers_missing).length ? Object.entries(rep.server.changes.headers_missing).map(([k, v]) => `${k} (${num(v)})`).join(', ') : 'none'],
            ['Changed', `${num(rep.server.changes.headers_changed)} header values, ${num(rep.server.changes.body_changed)} bodies, ${num(rep.server.changes.path_changed)} paths`],
            ['Rejected markers', num(rep.rejected_markers)],
          ])) : null,
        ...((rep.cluster && rep.cluster.warnings) || []).map((w) => notice('warn', w)),
        (rep.findings || []).length ? h('div', { class: 'stack-sm' }, h('h3', null, 'Findings'),
          ...rep.findings.map((f) => notice(f.failed && f.failed.length ? 'warn' : 'ok', f.summary))) : null,
        (rep.changes || []).length ? h('details', { class: 'disclosure', open: true }, h('summary', null, `Live changes (${rep.changes.length})`),
          h('ul', { class: 'small' }, ...rep.changes.map((c) => h('li', null, `${num(c.at_secs, 1)} s (v${c.version}): ${c.what}`)))) : null,
        (rep.stages || []).length > 1 ? h('details', { class: 'disclosure' }, h('summary', null, `Stages (${rep.stages.length}), the run's results in each`),
          h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
            h('thead', null, h('tr', null, h('th', null, 'Layer'), h('th', null, 'Stage'), h('th', null, 'Target'), h('th', null, 'From'), h('th', null, 'To'), h('th', null, 'Answered/s'), h('th', null, 'Errors'), h('th', null, 'p99 max'))),
            h('tbody', null, ...rep.stages.slice(0, 200).map((x) => h('tr', null, h('td', null, x.layer), h('td', null, stageLabel(x.type, x.stage, x.part)), h('td', { class: 'mono' }, num(x.target, 1)), h('td', { class: 'mono' }, `${num(x.from_secs, 1)} s`), h('td', { class: 'mono' }, `${num(x.to_secs, 1)} s`), h('td', { class: 'mono' }, num(x.rate, 1)), h('td', { class: x.errors ? 'err-text' : '' }, num(x.errors)), h('td', { class: 'mono' }, fmtMs(x.p99_ms_max)))))))) : null,
        rep.thresholds && rep.thresholds.length ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Threshold'), h('th', null, 'Limit'), h('th', null, 'Measured'), h('th', null, 'Result'))),
          h('tbody', null, ...rep.thresholds.map((th) => h('tr', null, h('td', null, th.name), h('td', { class: 'mono' }, String(th.limit)), h('td', { class: 'mono' }, String(th.measured)), h('td', null, badge(th.pass ? 'pass' : 'fail', th.pass ? 'ok' : 'err')))))) : h('p', { class: 'muted small' }, 'No thresholds in the scenario.'),
        rep.cluster ? h('p', { class: 'small' }, `Cluster ${rep.cluster.cluster_id}, led by ${rep.cluster.leader} (term ${rep.cluster.term}): ${rep.cluster.nodes.length} node(s), clock error bound ${fmtMs(rep.cluster.clock_error_ms)}, lost check ${rep.cluster.lost_check}.`) : null,
        timelineChart(liveBuckets),
        (rep.layers || []).length > 1 || (rep.targets_breakdown || []).length > 1 ? h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Breakdown'), h('th', null, 'Sent'), h('th', null, 'OK'), h('th', null, 'Errors'), h('th', null, 'p50'), h('th', null, 'p99'))),
          h('tbody', null, ...[...(rep.layers || []).map((b) => ['layer', b]), ...(rep.targets_breakdown || []).map((b) => ['target', b])].map(([kind, b]) => h('tr', null, h('td', null, `${kind} ${b.name}`), h('td', null, num(b.counts.sent)), h('td', null, num(b.counts.ok)), h('td', null, num(b.counts.unexpected_status + b.counts.transport)), h('td', null, fmtMs(b.latency.p50_ms)), h('td', null, fmtMs(b.latency.p99_ms)))))) : null,
        sat.reasons && sat.reasons.length ? notice('warn', sat.reasons.join(' ')) : null,
        h('details', { class: 'disclosure' }, h('summary', null, `Samples (${(rep.samples || []).length}) with their X-Request-Id`),
          h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
            h('thead', null, h('tr', null, h('th', null, 'Kind'), h('th', null, 'Class'), h('th', null, 'Request id'), h('th', null, 'Request'), h('th', null, 'Status'), h('th', null, 'Latency'), h('th', null, 'Upstream'))),
            h('tbody', null, ...(rep.samples || []).slice(0, 50).map((x) => h('tr', null, h('td', null, x.kind), h('td', null, x.class || '-'), h('td', { class: 'mono' }, x.request_id), h('td', { class: 'mono' }, `${x.method} ${x.path}`), h('td', null, x.status !== null && x.status !== undefined ? String(x.status) : (x.error || '-')), h('td', null, fmtMs(x.latency_ms)), h('td', null, x.upstream_ms !== undefined && x.upstream_ms !== null ? fmtMs(x.upstream_ms) : '-'))))))),
        h('details', { class: 'disclosure' }, h('summary', null, 'Full report (JSON)'), jsonView(rep))));
    }

    loadNode().then((n) => { if (n && n.enabled) loadBuilder(); else replace(builderEl, h('p', { class: 'muted small' }, 'Turn harness mode on to build and run scenarios.')); });
    if (st.runId) startFeed();
    const refresh = setInterval(loadNode, 15000);

    return () => {
      alive = false;
      stopFeed();
      clearTimeout(previewTimer);
      if (pollTimer) clearInterval(pollTimer);
      clearInterval(refresh);
      clear(root);
    };
  },
};
