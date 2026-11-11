// A mock's Drift tab (#/mocks/{name}/drift): check the mock against the real
// API it imitates (POST /_rustybin/mocks/{name}/drift, egress purpose
// record) and read the kept runs. The upstream is the mock's drift.upstream
// (saved to the draft); a credential for the real API goes in a header whose
// value may hold {{VAR}}, given here and never kept. The same on every
// surface: rustybin mocks drift, check_mock_drift.
import { h, replace, clear, card, field, input, notice, icon, spinner, toast, badge, fmtTime } from '../../lib/dom.js';
import { api, report } from '../../lib/mockapi.js';
import { send, SERVER } from '../../lib/http.js';

const enc = encodeURIComponent;

export function driftPanel(ctx) {
  const path = `/_rustybin/mocks/${enc(ctx.name)}/drift`;
  const upstream = input({ id: 'drift-upstream', placeholder: 'https://api.example.com/v1', 'aria-label': 'Real API base URL' });
  const headerName = input({ id: 'drift-header-name', placeholder: 'Authorization', 'aria-label': 'Header name' });
  const headerValue = input({ id: 'drift-header-value', type: 'password', placeholder: 'Bearer ...', 'aria-label': 'Header value' });
  const resultEl = h('div', { class: 'stack' });
  const runsEl = h('div', { class: 'stack-sm' });
  const checkBtn = h('button', { class: 'btn primary', type: 'button', id: 'drift-check' }, icon('send'), 'Check against the real API');
  const saveBtn = ctx.readOnly ? null : h('button', { class: 'btn sm', type: 'button', id: 'drift-save' }, 'Save as the mock\'s upstream');
  let loaded = false;

  async function call(method, body) {
    const opts = { control: true, admin: method !== 'GET', method, url: SERVER + path, timeout: 90000 };
    if (body !== undefined) {
      opts.headers = { 'Content-Type': 'application/json' };
      opts.body = JSON.stringify(body);
    }
    const r = await send(opts);
    if (r.error) throw new Error(r.error);
    if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
    return r.json;
  }

  function runView(run) {
    const rows = run.results.map((r) => h('tr', null,
      h('td', null, badge(r.ok ? 'ok' : 'drift', r.ok ? 'ok' : 'err')),
      h('td', { class: 'small mono' }, `${r.method} ${r.path}`),
      h('td', { class: 'small' }, r.status === undefined ? 'no answer' : String(r.status)),
      h('td', { class: 'small' }, ...(r.findings.length ? r.findings.map((f) => h('div', null, h('strong', null, f.kind), f.warning ? ' (warning) ' : ' ', f.detail)) : ['matches the spec']))));
    return h('div', { class: 'stack-sm' },
      notice(run.passed ? 'ok' : 'err', run.passed
        ? `The mock matches the real API: ${run.operations} operation(s) checked.`
        : `The mock drifted from the real API in ${run.drifted} of ${run.operations} operation(s)${run.skipped ? `, ${run.skipped} not checked in time` : ''}.`),
      h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'drift-results' },
        h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Operation'), h('th', null, 'Status'), h('th', null, 'Findings'))),
        h('tbody', null, rows))),
      h('p', { class: 'muted small' }, `Checked ${fmtTime(run.checked_at)} against ${run.upstream} in ${run.ms} ms.`));
  }

  async function loadRuns() {
    try {
      const d = await call('GET');
      clear(runsEl);
      if (!d.runs.length) runsEl.appendChild(h('p', { class: 'muted small' }, 'No drift check yet.'));
      for (const run of d.runs) {
        runsEl.appendChild(h('div', { class: 'row' }, badge(run.passed ? 'matches' : 'drifted', run.passed ? 'ok' : 'err'),
          h('span', { class: 'small' }, `${fmtTime(run.checked_at)}: ${run.drifted} of ${run.operations} operation(s) drifted, ${run.warnings} warning(s)`)));
      }
    } catch (e) {
      replace(runsEl, notice('err', e.message));
    }
  }

  checkBtn.onclick = async () => {
    const body = {};
    if (upstream.value.trim()) body.upstream = upstream.value.trim();
    if (headerName.value.trim() && headerValue.value) body.headers = { [headerName.value.trim()]: headerValue.value };
    checkBtn.disabled = true;
    replace(resultEl, spinner('Sending one request per operation to the real API'));
    try {
      replace(resultEl, runView(await call('POST', body)));
      loadRuns();
    } catch (e) {
      replace(resultEl, notice('err', e.message));
    } finally {
      checkBtn.disabled = false;
    }
  };

  if (saveBtn) {
    saveBtn.onclick = async () => {
      const d = ctx.detail();
      const value = upstream.value.trim();
      const rr = await api('PATCH', '/' + enc(ctx.name), { drift: value ? { upstream: value } : null }, { revision: d ? d.draft_revision : undefined });
      if (report(rr, 'Saved to the draft.', 'Saving')) ctx.refresh();
    };
  }

  const el = h('div', { class: 'stack', id: 'drift-panel' },
    card('Drift', { hint: 'the mock against the real API it imitates' },
      h('p', { class: 'small muted', style: { margin: 0 } }, 'Sends one valid request per operation to the real API (its host must be allowed for the outbound purpose record) and reports where its answers disagree with the spec: a status it does not declare, a body that breaks the schema, a missing or undocumented field, an operation that is gone. The header is sent with the check and never kept.'),
      h('div', { class: 'row' }, field('Real API base URL', upstream), saveBtn),
      h('div', { class: 'row' }, field('Header (optional)', headerName), field('Value', headerValue)),
      h('div', { class: 'row' }, checkBtn),
      resultEl),
    card('Kept checks', { hint: 'the last 10' }, runsEl));

  return {
    el,
    show() {
      const d = ctx.detail();
      const saved = d && d.settings && d.settings.drift ? d.settings.drift.upstream : '';
      if (saved && !upstream.value) upstream.value = saved;
      if (!loaded) { loaded = true; loadRuns(); }
    },
  };
}
