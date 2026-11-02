// Saved tests: captured requests (Live traffic, "Save as test") or written
// ones with assertions, run here (in process) or through their gateway, and
// exported as Hurl for CI. GET /_rustybin/tests; saving, running and
// deleting are admin. The same on every surface: rustybin tests, the
// *_test(s) tools, the tests: section of rustybin.yaml.
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

    async function call(method, path, body) {
      const opts = { control: true, admin: true, method, url: SERVER + path };
      if (body !== undefined) {
        opts.headers = { 'Content-Type': 'application/json' };
        opts.body = JSON.stringify(body);
      }
      const r = await send(opts);
      if (r.error) throw new Error(r.error);
      if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
      return r.json !== undefined ? r.json : r.text;
    }

    async function load() {
      let d;
      try {
        d = await getJson(BASE);
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
        const r = await call('POST', BASE + '/run', { names: names || [], variables: parseVars(vars.value) });
        for (const t of r.results) last.set(t.name, t);
        replace(resultEl, report(r));
        await load();
      } catch (e) {
        replace(resultEl, notice('err', e.message));
      }
    }

    function report(r) {
      const rows = r.results.filter((t) => !t.passed).map((t) => h('div', { class: 'stack-sm', 'data-result': t.name },
        h('strong', null, t.name),
        t.error ? notice('warn', t.error) : h('ul', { class: 'small' }, ...t.failures.map((f) => h('li', null, `${f.assertion}: expected ${f.expected}, saw ${f.seen}`)))));
      return h('div', { class: 'stack' },
        notice(r.passed ? 'ok' : 'err', r.passed ? `All ${r.total} test(s) passed.` : `${r.failed} failed, ${r.errors} could not run, of ${r.total}.`),
        ...rows,
        h('p', { class: 'muted small' }, `Ran at ${fmtTime(r.ran_at)} in ${r.duration_ms} ms`));
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
        const href = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
        const a = h('a', { href, download: 'rustybin-tests.hurl' });
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(href), 60000);
      } catch (e) {
        toast('Export failed: ' + e.message, 'err');
      }
    };

    load();
    return () => { alive = false; };
  },
};
