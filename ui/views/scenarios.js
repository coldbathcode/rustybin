// Gateway scenarios: pick an outcome (HTTP: retries, rate limiting,
// credential injection, timeouts, caching, circuit breakers, request size
// limits, CORS, header transformation; AI: token limits, model fallback,
// semantic caching, prompt and response guards, load balancing; MCP: tool
// poisoning, rug pull, shadowing, result injection, oversized results,
// exfiltration links), prepare
// its upstream, follow the steps on the gateway, check through it and clean
// up. GET /_rustybin/scenarios; the run routes are admin. A run's test
// parameters are fields from its `parameters`, by type (a number, a
// checkbox, a choice or text). The same on every surface: rustybin
// scenario, the *_gateway_scenario tools.
import { h, replace, clear, card, field, input, select, checkbox, notice, icon, badge, spinner, toast, fmtTime } from '../lib/dom.js';
import { copyableCode } from '../lib/format.js';
import { getJson, send, SERVER } from '../lib/http.js';

const BASE = '/_rustybin/scenarios';

export default {
  id: 'scenarios',
  mount(root) {
    let alive = true;
    const gateways = new Map();
    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Gateway scenarios'),
        h('p', null, 'Prove a gateway feature end to end: Rustybin prepares the upstream, says how to point the gateway at it, sends test traffic through the gateway and compares what the client got with what reached the upstream. Ending a run removes only what it created.'))));
    const catalogEl = h('div', { id: 'scenario-catalog' }, spinner('Loading the scenarios'));
    const runsEl = h('div', { id: 'scenario-runs', class: 'stack' });
    root.appendChild(card('Scenarios', { hint: 'GET ' + BASE }, catalogEl));
    root.appendChild(h('h2', { class: 'section-title' }, 'Your runs'));
    root.appendChild(runsEl);

    async function call(method, path, body) {
      const opts = { control: true, admin: true, method, url: SERVER + path };
      if (body !== undefined) {
        opts.headers = { 'Content-Type': 'application/json' };
        opts.body = JSON.stringify(body);
      }
      const r = await send(opts);
      if (r.error) throw new Error(r.error);
      if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
      return r.json;
    }

    async function load() {
      let d;
      try {
        d = await getJson(BASE);
      } catch (e) {
        replace(catalogEl, notice('err', 'The scenarios could not be loaded: ' + e.message));
        return;
      }
      if (!alive) return;
      const row = (s) => h('div', { class: 'row', 'data-scenario': s.id },
        h('div', { class: 'grow' }, h('strong', null, s.title), h('div', { class: 'muted small' }, s.proves)),
        h('button', { class: 'btn sm', type: 'button', id: 'scenario-start-' + s.id, onclick: () => start(s.id) }, icon('plus'), 'Prepare'));
      const groups = [['http', 'HTTP gateways'], ['ai', 'AI gateways'], ['mcp', 'MCP gateways']];
      replace(catalogEl, h('div', { class: 'stack' }, ...groups.flatMap(([g, title]) => {
        const items = d.scenarios.filter((s) => (s.group || 'http') === g);
        return items.length ? [h('h3', { class: 'section-title', 'data-group': g }, title), ...items.map(row)] : [];
      })));
      clear(runsEl);
      if (!d.runs.length) runsEl.appendChild(h('p', { class: 'muted' }, 'No runs in this session. Prepare a scenario above.'));
      for (const run of d.runs) runsEl.appendChild(runCard(run));
    }

    async function start(id) {
      try {
        await call('POST', `${BASE}/${encodeURIComponent(id)}/runs`);
        toast('Run prepared: follow the steps on the gateway');
        await load();
      } catch (e) {
        toast('Could not prepare: ' + e.message, 'err');
      }
    }

    function runCard(run) {
      const path = `${BASE}/${encodeURIComponent(run.scenario)}/runs/${encodeURIComponent(run.id)}`;
      const verdictEl = h('div', { class: 'scenario-verdict' }, run.last_check ? verdictView(run.last_check) : null);
      const gw = input({ id: 'gateway-' + run.id, placeholder: 'https://gateway.example.com/my-route', value: gateways.get(run.id) || '' });
      gw.addEventListener('input', () => gateways.set(run.id, gw.value));
      // One field per parameter, by its type: a number, a checkbox or text.
      const params = (run.parameters || []).map((p) => {
        const id = `${p.name.replace(/_/g, '-')}-${run.id}`;
        const label = p.label || p.name;
        if (p.type === 'boolean') {
          const cb = checkbox(label, !!p.default);
          cb.input.id = id;
          cb.input.title = p.description;
          return { name: p.name, type: p.type, el: cb.input, view: cb.el };
        }
        if (Array.isArray(p.enum)) {
          const el = select(p.enum, p.default, { id, title: p.description });
          return { name: p.name, type: 'choice', el, view: field(label, el) };
        }
        const el = p.type === 'integer'
          ? input({ id, type: 'number', min: p.minimum, max: p.maximum, placeholder: String(p.default), title: p.description })
          : input({ id, maxlength: p.maxLength, placeholder: p.default === undefined ? 'any' : String(p.default), title: p.description });
        return { name: p.name, type: p.type, el, view: field(label, el) };
      });
      const checkBtn = h('button', { class: 'btn primary', type: 'button', id: 'check-' + run.id }, icon('send'), 'Check through the gateway');
      checkBtn.onclick = async () => {
        const body = { gateway_url: gw.value.trim() };
        for (const p of params) {
          if (p.type === 'boolean') {
            if (p.el.checked) body[p.name] = true;
          } else if (p.type === 'choice') {
            if (p.el.value) body[p.name] = p.el.value;
          } else if (p.el.value.trim()) {
            body[p.name] = p.type === 'integer' ? Number(p.el.value) : p.el.value.trim();
          }
        }
        checkBtn.disabled = true;
        replace(verdictEl, spinner('Sending the test requests through the gateway'));
        try {
          replace(verdictEl, verdictView(await call('POST', path + '/check', body)));
        } catch (e) {
          replace(verdictEl, notice('err', e.message));
        } finally {
          checkBtn.disabled = false;
        }
      };
      const endBtn = h('button', { class: 'btn sm', type: 'button', id: 'end-' + run.id }, icon('trash'), 'Clean up');
      endBtn.onclick = async () => {
        if (!window.confirm(`End this ${run.title.toLowerCase()} run? What it created (a mock, counters, an AI profile) is removed.`)) return;
        try {
          const r = await call('DELETE', path);
          toast(`Ended: removed ${r.removed.length} resource(s)`);
          await load();
        } catch (e) {
          toast('Could not end: ' + e.message, 'err');
        }
      };
      const el = card(run.title + ' run ' + run.id, { hint: 'Expires ' + fmtTime(run.expires_at), actions: endBtn },
        h('p', { class: 'small' }, run.second_upstream ? 'Upstream targets for the gateway route:' : 'Upstream for the gateway route:'),
        copyableCode(run.upstream),
        run.second_upstream ? copyableCode(run.second_upstream) : null,
        h('ol', { class: 'small' }, ...run.setup.map((s) => h('li', null, s))),
        h('div', { class: 'row' }, field('Gateway URL of the route', gw), ...params.map((p) => p.view)),
        h('div', { class: 'row' }, checkBtn),
        verdictEl);
      el.setAttribute('data-run', run.id);
      return el;
    }

    function verdictView(v) {
      const rows = v.expectations.map((e) => h('tr', null,
        h('td', null, badge(e.ok ? 'ok' : 'fail', e.ok ? 'ok' : 'err')),
        h('td', { class: 'small' }, e.name),
        h('td', { class: 'small' }, e.expected),
        h('td', { class: 'small' }, e.seen)));
      return h('div', { class: 'stack' },
        notice(v.passed ? 'ok' : 'err', v.passed ? 'The gateway passed this scenario.' : 'The gateway did not pass: see the failed expectations.'),
        h('div', { class: 'table-wrap' }, h('table', { class: 'table scenario-expectations' },
          h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Expectation'), h('th', null, 'Expected'), h('th', null, 'Seen'))),
          h('tbody', null, rows))),
        ...(v.hints || []).map((t) => notice('warn', t)),
        h('p', { class: 'muted small' }, `Check ${v.check} at ${fmtTime(v.checked_at)} through ${v.gateway_url}`));
    }

    load();
    return () => { alive = false; };
  },
};
