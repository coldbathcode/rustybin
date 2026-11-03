// Saved tests: captured requests (Live traffic, "Save as test") or written
// ones with assertions, run here (in process) or through their gateway, and
// exported as Hurl for CI. GET /_rustybin/tests; saving, running and
// deleting are admin. The same on every surface: rustybin tests, the
// *_test(s) tools, the tests: section of rustybin.yaml. The History card is
// GET /_rustybin/tests/runs (rustybin tests runs, list_test_runs): every
// run kept, a run's detail, the change since the previous run, its JUnit.
import { h, replace, card, notice, icon, badge, spinner, toast, methodBadge, fmtTime } from '../lib/dom.js';
import { getJson, send, SERVER } from '../lib/http.js';

const BASE = '/_rustybin/tests';

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
    const runAll = h('button', { class: 'btn primary', type: 'button', id: 'tests-run' }, icon('play'), 'Run all');
    const exportBtn = h('button', { class: 'btn', type: 'button', id: 'tests-export' }, icon('download'), 'Export as Hurl');
    const resultEl = h('div', { id: 'tests-result' });
    root.appendChild(card('Tests', { hint: 'GET ' + BASE, actions: h('div', { class: 'row' }, exportBtn, runAll) },
      listEl,
      h('label', { class: 'field' }, h('span', null, 'Variables for this run'), vars),
      resultEl));

    // The history: every run from any surface, newest first, a run's
    // detail and its JUnit report.
    const historyEl = h('div', { id: 'tests-history' }, spinner('Loading the history'));
    const detailEl = h('div', { id: 'tests-run-detail' });
    const moreBtn = h('button', { class: 'btn sm', type: 'button', id: 'tests-history-more', hidden: true }, 'Older runs');
    const refreshBtn = h('button', { class: 'btn', type: 'button', id: 'tests-history-refresh', title: 'Refresh' }, icon('refresh'));
    const clearBtn = h('button', { class: 'btn', type: 'button', id: 'tests-history-clear' }, icon('trash'), 'Clear history');
    root.appendChild(card('History', { hint: 'GET ' + BASE + '/runs', actions: h('div', { class: 'row' }, refreshBtn, clearBtn) },
      h('p', { class: 'muted small' }, 'Every run, from the console, the CLI, the API or an agent: the last 200 for 30 days (50 without a state directory). Variables are never kept.'),
      historyEl, moreBtn, detailEl));
    let runs = [];
    let cursor = null;

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
        d = await getJson(BASE, { admin: true, session: false });
      } catch (e) {
        replace(listEl, notice('err', 'The tests could not be loaded: ' + e.message));
        return;
      }
      if (!alive) return;
      if (!d.tests.length) {
        replace(listEl, h('p', { class: 'muted' }, 'No saved tests yet. Open Live traffic, pick a request and choose "Save as test".'));
        runAll.disabled = true;
        return;
      }
      runAll.disabled = false;
      const rows = d.tests.map((t) => {
        const res = last.get(t.name);
        return h('tr', { 'data-test': t.name },
          h('td', null, res ? badge(res.passed ? 'pass' : res.error ? 'error' : 'fail', res.passed ? 'ok' : 'err') : badge('not run', 'outline')),
          h('td', { class: 'mono small' }, t.name),
          h('td', null, methodBadge(t.request.method), ' ', h('span', { class: 'mono small' }, t.request.path)),
          h('td', { class: 'small' }, t.gateway ? 'through ' + t.gateway : 'this instance'),
          h('td', { class: 'small' }, String((t.assert || []).length)),
          h('td', { class: 'mono small' }, (t.variables || []).join(', ')),
          h('td', null, h('div', { class: 'row nowrap' },
            h('button', { class: 'btn sm', type: 'button', title: 'Run this test', onclick: () => run([t.name]) }, icon('play')),
            h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'data-delete': t.name, onclick: () => remove(t.name) }, icon('trash')))));
      });
      replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'tests-table' },
        h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Name'), h('th', null, 'Request'), h('th', null, 'Target'), h('th', null, 'Assertions'), h('th', null, 'Variables'), h('th', null, ''))),
        h('tbody', null, rows))));
    }

    async function run(names) {
      replace(resultEl, spinner('Running'));
      try {
        // The header labels the run's trigger in the history (never trusted).
        const r = await call('POST', BASE + '/run', { names: names || [], variables: parseVars(vars.value) }, { 'X-Rustybin-Client': 'console' });
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

    function report(r) {
      const rows = r.results.filter((t) => !t.passed).map((t) => h('div', { class: 'stack-sm', 'data-result': t.name },
        h('strong', null, t.name),
        t.error ? notice('warn', t.error) : h('ul', { class: 'small' }, ...t.failures.map((f) => h('li', null, `${f.assertion}: expected ${f.expected}, saw ${f.seen}`)))));
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
        replace(historyEl, notice('err', 'The history could not be loaded: ' + e.message));
        return;
      }
      if (!alive) return;
      runs = more ? runs.concat(d.runs) : d.runs;
      cursor = d.next_cursor || null;
      moreBtn.hidden = !cursor;
      clearBtn.disabled = !runs.length;
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

    load();
    loadHistory();
    return () => { alive = false; };
  },
};
