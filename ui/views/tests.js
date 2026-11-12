// Saved tests: captured requests (Live traffic, "Save as test") or written
// ones with assertions, run here (in process) or through their gateway, and
// exported as Hurl for CI. GET /_rustybin/tests; saving, running and
// deleting are admin. The same on every surface: rustybin tests, the
// *_test(s) tools, the tests: section of rustybin.yaml. The History card is
// GET /_rustybin/tests/runs (rustybin tests runs, list_test_runs): every
// run kept, a run's detail, the change since the previous run, its JUnit.
// The Schedules card is GET /_rustybin/tests/schedules (rustybin tests
// schedules, list_test_schedules, test_schedules: in rustybin.yaml): the
// tests every N minutes or at a time of day, a webhook for failures.
import { h, replace, card, notice, icon, badge, spinner, toast, methodBadge, fmtTime } from '../lib/dom.js';
import { getJson, send, SERVER } from '../lib/http.js';
import * as perms from '../lib/perms.js';
import * as projects from '../lib/projects.js';

const BASE = '/_rustybin/tests';

/** Why a card has nothing to show: refused is a fact of this caller, not a fault. */
function couldNotLoad(what, e) {
  if (/^40[13] /.test(e.message)) return notice('', 'Saved tests belong to the operator of this instance: sign in with rights to them to see them here.');
  return notice('err', what + ' could not be loaded: ' + e.message);
}

function parseVars(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const t = line.trim();
    if (!t) continue;
    const i = t.indexOf('=');
    if (i > 0) out[t.slice(0, i).trim()] = t.slice(i + 1);
  }
  return out;
}

export default {
  id: 'tests',
  mount(root) {
    let alive = true;
    let last = new Map();
    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Saved tests'),
        h('p', null, 'Requests with assertions, saved from Live traffic ("Save as test") or written out. Run them against this instance or through a gateway, and export them as Hurl for CI. Credentials are never saved: they become variables you fill when you run.'))));
    const listEl = h('div', { id: 'tests-list' }, spinner('Loading the saved tests'));
    const vars = h('textarea', { class: 'input mono', id: 'tests-vars', rows: 3, placeholder: 'NAME=value, one per line (not saved)' });
    const runAll = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'tests-run' }, icon('play'), 'Run all'), 'tests.run');
    const exportBtn = h('button', { class: 'btn', type: 'button', id: 'tests-export' }, icon('download'), 'Export as Hurl');
    const resultEl = h('div', { id: 'tests-result' });
    root.appendChild(card('Tests', { hint: 'GET ' + BASE, actions: h('div', { class: 'row' }, exportBtn, runAll) },
      listEl,
      h('label', { class: 'field' }, h('span', null, 'Variables for this run'), vars),
      resultEl));

    // Arazzo: a document of multi-step workflows, each imported as a
    // multi-step test against this instance's mocks.
    const arazzoText = h('textarea', { class: 'input mono', id: 'arazzo-document', rows: 8, spellcheck: 'false', placeholder: 'arazzo: 1.0.1\nsourceDescriptions:\n  - { name: petstore, url: ./petstore.yaml, type: openapi }\nworkflows:\n  - workflowId: create-and-read\n    steps: [...]' });
    const arazzoFile = h('input', { type: 'file', id: 'arazzo-file', accept: '.yaml,.yml,.json', class: 'input' });
    const arazzoGateway = h('input', { class: 'input mono', id: 'arazzo-gateway', placeholder: 'https://gateway.example.com/pets (optional: absent, this instance)' });
    const arazzoReplace = h('input', { type: 'checkbox', id: 'arazzo-replace' });
    const arazzoPreview = perms.guard(h('button', { class: 'btn', type: 'button', id: 'arazzo-preview' }, 'Preview'), 'tests.write');
    const arazzoImport = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'arazzo-import' }, icon('upload'), 'Import'), 'tests.write');
    const arazzoResult = h('div', { id: 'arazzo-result' });
    arazzoFile.onchange = async () => {
      const f = arazzoFile.files && arazzoFile.files[0];
      if (f && f.size <= 1024 * 1024) arazzoText.value = await f.text();
      else if (f) replace(arazzoResult, notice('err', 'An Arazzo document is at most 1 MiB.'));
    };
    arazzoPreview.onclick = () => importArazzo(true);
    arazzoImport.onclick = () => importArazzo(false);
    root.appendChild(card('Import Arazzo', { hint: 'POST ' + BASE + '/import' },
      h('p', { class: 'muted small' }, 'An Arazzo document describes journeys of several steps. Each workflow becomes one multi-step test: its source descriptions are this instance\'s mocks (nothing is fetched), outputs of a step feed the next, and what has no counterpart here is listed, never guessed.'),
      h('label', { class: 'field' }, h('span', null, 'An Arazzo file'), arazzoFile),
      h('label', { class: 'field' }, h('span', null, 'Or paste it'), arazzoText),
      h('label', { class: 'field' }, h('span', null, 'Gateway for the imported tests'), arazzoGateway),
      h('div', { class: 'row' }, h('label', { class: 'row small' }, arazzoReplace, 'Replace tests of the same names'), h('div', { class: 'grow' }), arazzoPreview, arazzoImport),
      arazzoResult));

    // The history: every run from any surface, newest first, a run's
    // detail and its JUnit report.
    const historyEl = h('div', { id: 'tests-history' }, spinner('Loading the history'));
    const detailEl = h('div', { id: 'tests-run-detail' });
    const moreBtn = h('button', { class: 'btn sm', type: 'button', id: 'tests-history-more', hidden: true }, 'Older runs');
    const refreshBtn = h('button', { class: 'btn', type: 'button', id: 'tests-history-refresh', title: 'Refresh' }, icon('refresh'));
    const clearBtn = perms.guard(h('button', { class: 'btn', type: 'button', id: 'tests-history-clear' }, icon('trash'), 'Clear history'), 'tests.write');
    root.appendChild(card('History', { hint: 'GET ' + BASE + '/runs', actions: h('div', { class: 'row' }, refreshBtn, clearBtn) },
      h('p', { class: 'muted small' }, 'Every run, from the console, the CLI, the API or an agent: the last 200 for 30 days (50 without a state directory). Variables are never kept.'),
      historyEl, moreBtn, detailEl));
    let runs = [];
    let cursor = null;

    // Schedules: the tests run every N minutes or at a time of day (UTC),
    // with a webhook for failures. GET /_rustybin/tests/schedules.
    const schedulesEl = h('div', { id: 'schedules-list' }, spinner('Loading the schedules'));
    const sName = h('input', { class: 'input mono', id: 'schedule-name', placeholder: 'nightly', maxlength: 64 });
    const sWhen = h('select', { class: 'input', id: 'schedule-when' }, h('option', { value: 'every' }, 'Every'), h('option', { value: 'at' }, 'Daily at (UTC)'));
    const sValue = h('input', { class: 'input mono', id: 'schedule-value', placeholder: '15m (5m to 24h)', value: '1h' });
    const sSelect = h('select', { class: 'input', id: 'schedule-select' }, h('option', { value: 'all' }, 'Every saved test'), h('option', { value: 'names' }, 'These tests'), h('option', { value: 'prefix' }, 'Tests named with a prefix'));
    const sTests = h('input', { class: 'input mono', id: 'schedule-tests', placeholder: 'order-1, order-2', hidden: true });
    const sHook = h('input', { class: 'input mono', id: 'schedule-webhook', placeholder: 'https://hooks.example.com/rustybin (optional)' });
    const sRecovery = h('input', { type: 'checkbox', id: 'schedule-recovery' });
    const sEnabled = h('input', { type: 'checkbox', id: 'schedule-enabled', checked: true });
    const sSave = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'schedule-save' }, icon('check'), 'Save schedule'), 'tests.write');
    const sReset = h('button', { class: 'btn', type: 'button', id: 'schedule-reset' }, 'New');
    const sResult = h('div', { id: 'schedule-result' });
    sWhen.onchange = () => { sValue.placeholder = sWhen.value === 'at' ? '02:30 (HH:MM, UTC)' : '15m (5m to 24h)'; };
    sSelect.onchange = () => { sTests.hidden = sSelect.value === 'all'; sTests.placeholder = sSelect.value === 'prefix' ? 'order-' : 'order-1, order-2'; };
    const scheduleForm = h('div', { class: 'stack', id: 'schedule-form' },
      h('h3', { class: 'section-title' }, 'Add or change a schedule'),
      h('div', { class: 'row' },
        h('label', { class: 'field grow' }, h('span', null, 'Name'), sName),
        h('label', { class: 'field' }, h('span', null, 'When'), sWhen),
        h('label', { class: 'field' }, h('span', null, 'Interval or time'), sValue)),
      h('div', { class: 'row' },
        h('label', { class: 'field' }, h('span', null, 'Tests'), sSelect),
        h('label', { class: 'field grow' }, h('span', null, 'Names or prefix'), sTests)),
      h('label', { class: 'field' }, h('span', null, 'Failure webhook'), sHook),
      h('div', { class: 'row' },
        h('label', { class: 'row small' }, sEnabled, 'On'),
        h('label', { class: 'row small' }, sRecovery, 'Notify when a failing schedule passes again'),
        h('div', { class: 'grow' }), sReset, sSave),
      h('p', { class: 'muted small' }, 'A failing run posts a JSON summary to the webhook. Allow the webhook\'s host under Management, Network (purpose notify); its bearer token is the notify.webhook_token setting, never part of the URL. Variables come from RUSTYBIN_TEST_* on the instance. A run due while the harness sends load is skipped.'),
      sResult);
    root.appendChild(card('Schedules', { hint: 'GET ' + BASE + '/schedules' }, schedulesEl, scheduleForm));

    async function call(method, path, body, extra) {
      const opts = { control: true, admin: true, method, url: SERVER + path };
      if (body !== undefined) {
        opts.headers = { 'Content-Type': 'application/json' };
        opts.body = JSON.stringify(body);
      }
      if (extra) opts.headers = Object.assign(opts.headers || {}, extra);
      const r = await send(opts);
      if (r.error) throw new Error(r.error);
      if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
      return r.json !== undefined ? r.json : r.text;
    }

    async function load() {
      let d;
      try {
        d = await getJson(BASE + projects.query(), { admin: true, session: false });
      } catch (e) {
        replace(listEl, couldNotLoad('The tests', e));
        return;
      }
      if (!alive) return;
      if (!d.tests.length) {
        replace(listEl, h('p', { class: 'muted' }, 'No saved tests yet. Open Live traffic, pick a request and choose "Save as test".'));
        runAll.disabled = true;
        return;
      }
      runAll.disabled = !perms.can('tests.run');
      const rows = d.tests.map((t) => {
        const res = last.get(t.name);
        return h('tr', { 'data-test': t.name },
          h('td', null, res ? badge(res.passed ? 'pass' : res.error ? 'error' : 'fail', res.passed ? 'ok' : 'err') : badge('not run', 'outline')),
          h('td', { class: 'mono small' }, t.name),
          (t.steps && t.steps.length)
            ? h('td', { 'data-steps': String(t.steps.length) }, badge(t.steps.length + ' steps', 'info'), ' ', h('span', { class: 'mono small' }, t.steps.map((s) => s.id).join(', ')))
            : h('td', null, methodBadge(t.request.method), ' ', h('span', { class: 'mono small' }, t.request.path)),
          h('td', { class: 'small' }, t.gateway ? 'through ' + t.gateway : 'this instance'),
          h('td', { class: 'small' }, String((t.steps && t.steps.length) ? t.steps.reduce((n, s) => n + (s.assert || []).length, 0) : (t.assert || []).length)),
          h('td', { class: 'mono small' }, (t.variables || []).join(', ')),
          h('td', null, h('div', { class: 'row nowrap' },
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Run this test', 'data-run-test': t.name, onclick: () => run([t.name]) }, icon('play')), 'tests.run'),
            (t.steps && t.steps.length) ? h('button', { class: 'btn sm', type: 'button', title: 'Export as an Arazzo document', 'data-arazzo': t.name, onclick: () => exportArazzo(t.name) }, icon('download')) : null,
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'data-delete': t.name, onclick: () => remove(t.name) }, icon('trash')), 'tests.write'))));
      });
      replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'tests-table' },
        h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Name'), h('th', null, 'Request'), h('th', null, 'Target'), h('th', null, 'Assertions'), h('th', null, 'Variables'), h('th', null, ''))),
        h('tbody', null, rows))));
    }

    async function run(names) {
      replace(resultEl, spinner('Running'));
      try {
        // The header labels the run's trigger in the history (never trusted).
        const r = await call('POST', BASE + '/run', { names: names || [], variables: parseVars(vars.value), project: projects.forNew() }, { 'X-Rustybin-Client': 'console' });
        for (const t of r.results) last.set(t.name, t);
        replace(resultEl, report(r));
        await load();
        await loadHistory();
      } catch (e) {
        replace(resultEl, notice('err', e.message));
      }
    }

    function changeLine(c) {
      if (!c || !c.previous) return h('p', { class: 'muted small', 'data-change': 'first' }, 'The first run kept: nothing to compare with.');
      const parts = [];
      if (c.newly_failing.length) parts.push(h('span', null, badge('newly failing', 'err'), ' ', h('span', { class: 'mono small' }, c.newly_failing.join(', '))));
      if (c.newly_passing.length) parts.push(h('span', null, badge('newly passing', 'ok'), ' ', h('span', { class: 'mono small' }, c.newly_passing.join(', '))));
      if (!parts.length) parts.push(h('span', { class: 'muted small' }, 'No change since the previous run.'));
      return h('div', { class: 'row', 'data-change': 'since' }, h('span', { class: 'small' }, 'Since ', h('span', { class: 'mono' }, c.previous), ':'), ...parts);
    }

    /** The steps of a multi-step test's result: each one's verdict, status, time and outputs. */
    function stepsTable(t) {
      return h('div', { class: 'table-wrap' }, h('table', { class: 'table', 'data-step-results': t.name },
        h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Step'), h('th', null, 'Status'), h('th', null, 'Time'), h('th', null, 'Outputs'), h('th', null, ''))),
        h('tbody', null, t.steps.map((s) => h('tr', { 'data-step-result': s.id },
          h('td', null, s.skipped ? badge('skipped', 'outline') : badge(s.passed ? 'pass' : s.error ? 'error' : 'fail', s.passed ? 'ok' : 'err')),
          h('td', { class: 'mono small' }, s.id),
          h('td', { class: 'small' }, s.status != null ? String(s.status) : ''),
          h('td', { class: 'small' }, s.ms != null ? `${s.ms} ms` : ''),
          h('td', { class: 'mono small' }, Object.entries(s.outputs || {}).map(([k, v]) => `${k}=${v}`).join(', ')),
          h('td', { class: 'small' }, s.error || (s.failures || []).map((f) => `${f.assertion}: expected ${f.expected}, saw ${f.seen}`).join('; ') || (s.skipped ? 'an earlier step failed' : '')))))));
    }

    function report(r) {
      const rows = r.results.filter((t) => !t.passed || (t.steps && t.steps.length)).map((t) => h('div', { class: 'stack-sm', 'data-result': t.name },
        h('strong', null, t.name),
        (t.steps && t.steps.length) ? stepsTable(t)
          : t.error ? notice('warn', t.error) : h('ul', { class: 'small' }, ...t.failures.map((f) => h('li', null, `${f.assertion}: expected ${f.expected}, saw ${f.seen}`)))));
      return h('div', { class: 'stack' },
        notice(r.passed ? 'ok' : 'err', r.passed ? `All ${r.total} test(s) passed.` : `${r.failed} failed, ${r.errors} could not run, of ${r.total}.`),
        r.change ? changeLine(r.change) : null,
        ...rows,
        h('p', { class: 'muted small' }, `Ran at ${fmtTime(r.ran_at)} in ${r.duration_ms} ms`, r.run_id ? ` (run ${r.run_id})` : ''));
    }

    async function loadHistory(more) {
      let d;
      try {
        const q = more && cursor ? `?cursor=${encodeURIComponent(cursor)}` : '';
        d = await getJson(BASE + '/runs' + q, { admin: true, session: false });
      } catch (e) {
        replace(historyEl, couldNotLoad('The history', e));
        return;
      }
      if (!alive) return;
      runs = more ? runs.concat(d.runs) : d.runs;
      cursor = d.next_cursor || null;
      moreBtn.hidden = !cursor;
      clearBtn.disabled = !runs.length || !perms.can('tests.write');
      if (!runs.length) {
        replace(historyEl, h('p', { class: 'muted' }, 'No runs yet. Run the tests above, or with rustybin tests run.'));
        return;
      }
      const rows = runs.map((r) => {
        const changed = r.change && r.change.previous
          ? [r.change.newly_failing.length ? badge(`${r.change.newly_failing.length} newly failing`, 'err') : null,
            r.change.newly_passing.length ? badge(`${r.change.newly_passing.length} newly passing`, 'ok') : null]
          : [badge('first', 'outline')];
        return h('tr', { 'data-run': r.id },
          h('td', null, badge(r.passed ? 'pass' : 'fail', r.passed ? 'ok' : 'err')),
          h('td', { class: 'small' }, fmtTime(r.ran_at)),
          h('td', { class: 'small' }, r.trigger),
          h('td', { class: 'mono small' }, r.principal),
          h('td', { class: 'small' }, `${r.total - r.failed - r.errors} of ${r.total}`),
          h('td', null, h('div', { class: 'row nowrap' }, ...changed)),
          h('td', null, h('div', { class: 'row nowrap' },
            h('button', { class: 'btn sm', type: 'button', title: 'Show this run', 'data-show-run': r.id, onclick: () => showRun(r.id) }, icon('info')),
            h('button', { class: 'btn sm', type: 'button', title: 'Download JUnit XML', 'data-junit-run': r.id, onclick: () => junit(r.id) }, icon('download')))));
      });
      replace(historyEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'tests-history-table' },
        h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'When'), h('th', null, 'Trigger'), h('th', null, 'Who'), h('th', null, 'Passed'), h('th', null, 'Since the previous run'), h('th', null, ''))),
        h('tbody', null, rows))));
    }

    async function showRun(id) {
      replace(detailEl, spinner('Loading the run'));
      try {
        const r = await call('GET', `${BASE}/runs/${encodeURIComponent(id)}`);
        const all = r.results.map((t) => h('tr', { 'data-run-result': t.name },
          h('td', null, badge(t.passed ? 'pass' : t.error ? 'error' : 'fail', t.passed ? 'ok' : 'err')),
          h('td', { class: 'mono small' }, t.name),
          h('td', { class: 'small' }, t.target),
          h('td', { class: 'small' }, t.status != null ? String(t.status) : ''),
          h('td', { class: 'small' }, t.ms != null ? `${t.ms} ms` : '')));
        replace(detailEl, card(`Run ${r.run_id}`, { hint: `${r.trigger}, ${r.principal}` },
          report(r),
          h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
            h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Test'), h('th', null, 'Target'), h('th', null, 'Status'), h('th', null, 'Time'))),
            h('tbody', null, all)))));
      } catch (e) {
        replace(detailEl, notice('err', e.message));
      }
    }

    async function exportArazzo(name) {
      try {
        const text = await call('GET', `${BASE}/${encodeURIComponent(name)}/export?format=arazzo`);
        download(text, `${name}.arazzo.yaml`, 'application/yaml');
      } catch (e) {
        toast('This test cannot be an Arazzo document: ' + e.message, 'err');
      }
    }

    async function importArazzo(dry) {
      if (!arazzoText.value.trim()) {
        replace(arazzoResult, notice('err', 'Paste an Arazzo document or choose its file.'));
        return;
      }
      replace(arazzoResult, spinner(dry ? 'Reading the document' : 'Importing'));
      try {
        const body = { document: arazzoText.value, dry_run: dry, replace: arazzoReplace.checked };
        if (arazzoGateway.value.trim()) body.gateway = arazzoGateway.value.trim();
        const project = projects.forNew();
        if (project && project !== projects.INSTANCE) body.project = project;
        const r = await call('POST', BASE + '/import', body);
        const sources = Object.entries(r.sources || {}).map(([k, v]) => `${k} is the mock ${v}`).join('; ');
        replace(arazzoResult, h('div', { class: 'stack-sm', id: 'arazzo-imported' },
          notice(dry ? 'info' : 'ok', dry
            ? `${r.tests.length} of ${r.workflows} workflow(s) would become tests: ${r.tests.map((t) => t.name).join(', ') || 'none'}.`
            : `Imported ${r.tests.length} test(s): ${r.tests.map((t) => t.name).join(', ')}.`),
          sources ? h('p', { class: 'small' }, 'Source descriptions: ' + sources + '.') : null,
          (r.left_out || []).length ? h('div', { class: 'stack-sm', id: 'arazzo-left-out' },
            h('p', { class: 'small' }, 'Left out (no counterpart here, never guessed):'),
            h('ul', { class: 'small mono' }, r.left_out.map((l) => h('li', null, l)))) : null));
        if (!dry) await load();
      } catch (e) {
        replace(arazzoResult, notice('err', e.message));
      }
    }

    async function junit(id) {
      try {
        const text = await call('GET', `${BASE}/runs/${encodeURIComponent(id)}?format=junit`);
        download(text, `rustybin-tests-${id}.xml`, 'application/xml');
      } catch (e) {
        toast('Download failed: ' + e.message, 'err');
      }
    }

    function download(text, name, type) {
      const href = URL.createObjectURL(new Blob([text], { type }));
      const a = h('a', { href, download: name });
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(href), 60000);
    }

    async function remove(name) {
      if (!window.confirm(`Delete the saved test ${name}?`)) return;
      try {
        await call('DELETE', `${BASE}/${encodeURIComponent(name)}`);
        last.delete(name);
        toast('Deleted ' + name);
        await load();
      } catch (e) {
        toast('Could not delete: ' + e.message, 'err');
      }
    }

    runAll.onclick = () => run(null);
    exportBtn.onclick = async () => {
      try {
        const text = await call('GET', BASE + '/export');
        download(text, 'rustybin-tests.hurl', 'text/plain');
      } catch (e) {
        toast('Export failed: ' + e.message, 'err');
      }
    };
    refreshBtn.onclick = () => loadHistory();
    moreBtn.onclick = () => loadHistory(true);
    clearBtn.onclick = async () => {
      if (!window.confirm('Forget every kept test run? The tests themselves stay.')) return;
      try {
        const r = await call('DELETE', BASE + '/runs');
        toast(`Forgot ${r.cleared} run(s)`);
        replace(detailEl);
        await loadHistory();
      } catch (e) {
        toast('Could not clear: ' + e.message, 'err');
      }
    };

    // ── Schedules ──

    let schedules = [];

    function whenOf(s) {
      return s.every ? `every ${s.every}` : `daily at ${s.at} UTC`;
    }

    function noticeBadge(n) {
      if (!n) return h('span', { class: 'muted small' }, 'none');
      const kind = { delivered: 'ok', pending: 'outline', retrying: 'warn', failed: 'err', refused: 'err', dropped: 'err' }[n.result] || 'outline';
      return h('span', { title: n.detail || '' }, badge(`${n.event}: ${n.result}`, kind));
    }

    async function loadSchedules() {
      let d;
      try {
        d = await getJson(BASE + '/schedules', { admin: true, session: false });
      } catch (e) {
        replace(schedulesEl, couldNotLoad('The schedules', e));
        return;
      }
      if (!alive) return;
      schedules = d.schedules;
      if (!schedules.length) {
        replace(schedulesEl, h('p', { class: 'muted' }, `No schedules yet. Save one below: the tests then run without anyone at the console (at most ${d.max_schedules}).`));
        return;
      }
      const rows = schedules.map((s) => {
        const st = s.status || {};
        const last = st.last_result
          ? h('span', { title: st.last_detail || '' }, badge(st.last_result, st.last_result === 'passed' ? 'ok' : st.last_result === 'skipped' ? 'warn' : 'err'),
            st.last_run_id ? h('button', { class: 'btn sm ghost', type: 'button', title: 'Show this run', onclick: () => showRun(st.last_run_id) }, icon('info')) : null)
          : badge('not run', 'outline');
        return h('tr', { 'data-schedule': s.name },
          h('td', { class: 'mono small' }, s.name),
          h('td', { class: 'small' }, whenOf(s)),
          h('td', { class: 'small' }, s.selection),
          h('td', null, h('label', { class: 'row small' },
            h('input', { type: 'checkbox', checked: s.enabled, 'data-toggle-schedule': s.name, onchange: (ev) => toggleSchedule(s, ev.target.checked) }),
            s.enabled ? 'on' : 'off')),
          h('td', { class: 'small' }, st.running ? badge('running', 'outline') : st.next_due ? fmtTime(st.next_due) : ''),
          h('td', null, last),
          h('td', null, s.webhook ? noticeBadge(st.last_notice) : h('span', { class: 'muted small' }, 'no webhook')),
          h('td', null, h('div', { class: 'row nowrap' },
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Run now', 'data-run-schedule': s.name, onclick: () => runSchedule(s.name) }, icon('play')), 'tests.run'),
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Edit', 'data-edit-schedule': s.name, onclick: () => editSchedule(s) }, icon('edit')), 'tests.write'),
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'data-delete-schedule': s.name, onclick: () => removeSchedule(s.name) }, icon('trash')), 'tests.write'))));
      });
      replace(schedulesEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'schedules-table' },
        h('thead', null, h('tr', null, h('th', null, 'Name'), h('th', null, 'When'), h('th', null, 'Tests'), h('th', null, 'On'), h('th', null, 'Next'), h('th', null, 'Last run'), h('th', null, 'Last notice'), h('th', null, ''))),
        h('tbody', null, rows))));
    }

    function formSchedule() {
      const name = sName.value.trim();
      const s = { name, enabled: sEnabled.checked, notify_recovery: sRecovery.checked };
      s[sWhen.value] = sValue.value.trim();
      const list = sTests.value.split(',').map((x) => x.trim()).filter(Boolean);
      if (sSelect.value === 'names') s.names = list;
      if (sSelect.value === 'prefix') s.prefix = sTests.value.trim();
      if (sHook.value.trim()) s.webhook = sHook.value.trim();
      return s;
    }

    function editSchedule(s) {
      sName.value = s.name;
      sWhen.value = s.every ? 'every' : 'at';
      sValue.value = s.every || s.at || '';
      sSelect.value = s.prefix ? 'prefix' : (s.names && s.names.length) ? 'names' : 'all';
      sTests.value = s.prefix || (s.names || []).join(', ');
      sTests.hidden = sSelect.value === 'all';
      sHook.value = s.webhook || '';
      sRecovery.checked = !!s.notify_recovery;
      sEnabled.checked = s.enabled !== false;
      sName.focus();
    }

    function resetForm() {
      sName.value = '';
      sWhen.value = 'every';
      sValue.value = '1h';
      sSelect.value = 'all';
      sTests.value = '';
      sTests.hidden = true;
      sHook.value = '';
      sRecovery.checked = false;
      sEnabled.checked = true;
      replace(sResult);
    }

    async function putSchedule(s) {
      const body = Object.assign({}, s);
      delete body.status;
      delete body.saved_at;
      delete body.selection;
      return call('PUT', `${BASE}/schedules/${encodeURIComponent(s.name)}`, body);
    }

    async function toggleSchedule(s, on) {
      try {
        await putSchedule(Object.assign({}, s, { enabled: on }));
        toast(`${s.name} is ${on ? 'on' : 'off'}`);
      } catch (e) {
        toast('Could not change it: ' + e.message, 'err');
      }
      await loadSchedules();
    }

    async function runSchedule(name) {
      replace(sResult, spinner('Running ' + name));
      try {
        const r = await call('POST', `${BASE}/schedules/${encodeURIComponent(name)}/run`);
        replace(sResult, r.run ? report(r.run) : notice('warn', `Skipped: ${r.detail}`));
        await loadSchedules();
        await loadHistory();
      } catch (e) {
        replace(sResult, notice('err', e.message));
      }
    }

    async function removeSchedule(name) {
      if (!window.confirm(`Delete the schedule ${name}? The tests and their history stay.`)) return;
      try {
        await call('DELETE', `${BASE}/schedules/${encodeURIComponent(name)}`);
        toast('Deleted ' + name);
        await loadSchedules();
      } catch (e) {
        toast('Could not delete: ' + e.message, 'err');
      }
    }

    sSave.onclick = async () => {
      const s = formSchedule();
      if (!s.name) {
        replace(sResult, notice('err', 'A schedule needs a name.'));
        return;
      }
      try {
        await putSchedule(s);
        replace(sResult, notice('ok', `Saved ${s.name}.`));
        await loadSchedules();
      } catch (e) {
        replace(sResult, notice('err', e.message));
      }
    };
    sReset.onclick = resetForm;

    load();
    loadHistory();
    loadSchedules();
    return () => { alive = false; };
  },
};
