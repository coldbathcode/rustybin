// Mock APIs: upload an OpenAPI spec and get a named mock at /mock/{name}.
// #/mocks lists the mocks (on/off switches, search, filters, import and
// export); #/mocks/{name} opens a mock's detail (views/mocks/detail.js).
import { h, replace, clear, card, icon, notice, spinner, empty, badge, input, toast, copyButton, toggleSwitch, segmented, debounce } from '../lib/dom.js';
import * as settings from '../lib/settings.js';
import { api, errorText, report, readOnlyReason, statePill, securityLabel, relTime, download, readFile, plural } from '../lib/mockapi.js';
import * as visit from '../lib/visit.js';
import { openWizard, sampleSpecText, wireDrop } from './mocks/wizard.js';
import { mountDetail } from './mocks/detail.js';
import { visitBar } from './mocks/visit.js';

const FILTERS = [['all', 'All'], ['on', 'On'], ['off', 'Off'], ['secured', 'Secured'], ['draft', 'Draft changes']];

function mountList(root) {
  let alive = true;
  let data = null;
  let timer = null;
  const st = settings.recall('mocks.list', { q: '', filter: 'all' });
  settings.remember('mocks.list', st);
  const readOnly = readOnlyReason();
  // Self-service (public instances): visitors create and change their own
  // mocks; the instance's stay read-only for them.
  let visitor = false;
  const canChange = (m) => !readOnly || !!m.yours;
  // Bulk selection (names), kept across the periodic refresh.
  const selected = new Set();
  const bulkEl = h('div', { class: 'bulk-bar row', role: 'region', 'aria-label': 'Bulk actions' });
  const noticeEl = h('div', { class: 'stack-sm' });
  // The visit bar, rebuilt only when the visit changes (not on every refresh).
  const visitEl = h('div');
  let visitKey = '';
  const heroEl = h('div');
  const listEl = h('div', null, spinner('Loading mocks'));
  const limitsEl = h('p', { class: 'tiny muted', style: { margin: 0 } });
  const search = input({ type: 'search', placeholder: 'Search name, title or URL', value: st.q, 'aria-label': 'Search mocks' });
  const filter = segmented(FILTERS, st.filter, (v) => { st.filter = v; renderList(); }, 'Filter mocks');
  search.addEventListener('input', debounce(() => { st.q = search.value.trim().toLowerCase(); renderList(); }, 100));
  const yamlIn = h('input', { type: 'file', accept: '.yaml,.yml,.json', hidden: true });
  const newBtn = h('button', { class: 'btn primary', type: 'button', id: 'new-mock', hidden: !!readOnly, onclick: () => wizard() }, icon('plus'), 'New mock');
  yamlIn.addEventListener('change', () => { if (yamlIn.files[0]) importYaml(yamlIn.files[0]); yamlIn.value = ''; });

  root.append(
    h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Mock APIs'),
        h('p', null, 'Upload an OpenAPI spec and get a mock of the API at its own URL: realistic responses, mocked security, request validation and live traffic. Point a gateway at it to demo the real thing before the backend exists.')),
      h('div', { class: 'row' },
        readOnly ? null : h('button', { class: 'btn', type: 'button', onclick: () => yamlIn.click(), title: 'Apply a rustybin.yaml document (creates or updates mocks)' }, icon('upload'), 'Import YAML'),
        h('button', { class: 'btn', type: 'button', onclick: exportAll, title: 'Every mock (except the sample) as rustybin.yaml' }, icon('download'), 'Export all'),
        newBtn,
        yamlIn)),
    visitEl, noticeEl, heroEl,
    card('Mocks', { actions: h('div', { class: 'row mock-filters' }, search, filter.el) }, readOnly ? null : bulkEl, listEl, limitsEl));

  function names() {
    return data ? data.mocks.map((m) => m.name) : [];
  }

  function wizard(text) {
    openWizard({
      taken: names(),
      text,
      maxBytes: data && data.limits ? data.limits.max_spec_bytes : 0,
      visitor: visitor ? data.self_service : null,
      onCreated: (d, deployed) => {
        toast(deployed ? `${d.name} is live` : `Draft ${d.name} created`);
        settings.remember('mocks.created', { name: d.name, deployed, token: !!d.manage_token });
        location.hash = '#/mocks/' + encodeURIComponent(d.name);
      },
    });
  }

  async function load() {
    const r = await api('GET', '');
    if (!alive) return;
    if (!r.ok || !r.json) {
      replace(listEl, notice('err', errorText(r, 'Loading the mocks'), ' ', h('button', { class: 'btn sm', type: 'button', onclick: load }, 'Retry')));
      return;
    }
    data = r.json;
    const ss = data.self_service || {};
    visitor = !!ss.enabled && !data.can_manage_instance;
    newBtn.hidden = !!readOnly && !visitor;
    // Held tokens without a running visit: the visit ended (its mocks went).
    if (visitor && visit.tokens().length && !data.visit) {
      visit.clearAll();
      toast('Your earlier visit has ended: its mocks were deleted.');
    }
    const l = data.limits || {};
    limitsEl.textContent = visitor
      ? `Up to ${ss.per_owner} mocks of your own, specs up to ${Math.round((l.max_spec_bytes || 0) / 1024)} KB, ${l.captures} captured calls kept per mock.`
      : `${data.count} of at most ${l.max_mocks} mocks. Specs up to ${Math.round((l.max_spec_bytes || 0) / 1024)} KB, ${l.revisions} revisions and ${l.captures} captured calls kept per mock` +
        (l.ttl_secs ? `, mocks expire after ${Math.round(l.ttl_secs / 60)} min.` : '.');
    clear(noticeEl);
    const v = visitor ? data.visit : null;
    const key = !visitor ? '' : v ? [v.expires_at, v.minutes_used, v.next_extension, v.extendable_at].join('|') : 'intro';
    if (key !== visitKey) {
      visitKey = key;
      replace(visitEl, !visitor ? null : v
        ? visitBar(ss, v, load)
        : notice('', h('strong', null, 'Try it with your own API. '),
          `Create up to ${ss.per_owner} mocks from your OpenAPI specs. They run for ${ss.visit_minutes} minutes, extendable to ${ss.anon_hours} hours` +
          (ss.leads ? `, and with an email address up to ${ss.daily_hours} hours a day` : '') + '.' +
          (ss.host ? ` Each one gets its own address on ${ss.host}.` : '')));
    }
    if (visitor) {
      // The visit bar above says it all.
    } else if (readOnly) {
      noticeEl.appendChild(notice('', readOnly));
    }
    if (!readOnly && settings.instance.adminTokenConfigured && !settings.get('adminToken')) {
      noticeEl.appendChild(notice('warn', 'This instance has an admin token: set it in the console settings to create and change mocks.'));
    }
    if ((data.load_errors || []).length) {
      noticeEl.appendChild(notice('err', h('strong', null, 'Some mocks from the config file or directory could not be loaded:'),
        h('ul', { class: 'issue-list small' }, ...data.load_errors.map((e) => h('li', null, e)))));
    }
    renderHero();
    renderList();
  }

  function renderHero() {
    const own = data.mocks.filter((m) => (visitor ? m.yours : m.origin !== 'sample'));
    if (own.length || (readOnly && !visitor)) { replace(heroEl); return; }
    const sample = data.mocks.find((m) => m.origin === 'sample');
    const drop = h('div', { class: 'dropzone hero', tabindex: '0', role: 'button', 'aria-label': 'Create a mock: drop or choose an OpenAPI file' },
      icon('upload'),
      h('strong', { class: 'hero-title' }, 'Drop an OpenAPI file, paste it, or try the Petstore sample'),
      h('span', { class: 'muted' }, 'OpenAPI 3.0 / 3.1 or Swagger 2.0, JSON or YAML. You get a live mock URL in three steps.'),
      h('div', { class: 'row', style: { justifyContent: 'center' } },
        h('button', { class: 'btn primary', type: 'button', onclick: (e) => { e.stopPropagation(); fileIn.click(); } }, icon('file'), 'Choose a file'),
        h('button', { class: 'btn', type: 'button', onclick: (e) => { e.stopPropagation(); wizard(); } }, 'Paste a spec'),
        h('button', { class: 'btn', type: 'button', onclick: async (e) => { e.stopPropagation(); try { wizard(await sampleSpecText()); } catch (err) { toast(err.message, 'err'); } } }, 'Use the Petstore sample')),
      sample ? h('span', { class: 'small muted' }, 'Or just explore the built-in ', h('a', { href: '#/mocks/' + sample.name, onclick: (e) => e.stopPropagation() }, sample.name), ' mock below.') : null);
    const fileIn = h('input', { type: 'file', accept: '.json,.yaml,.yml', hidden: true });
    const fromFile = async (f) => {
      try { wizard(await readFile(f, data.limits.max_spec_bytes)); } catch (err) { toast(err.message, 'err'); }
    };
    fileIn.addEventListener('change', () => { if (fileIn.files[0]) fromFile(fileIn.files[0]); fileIn.value = ''; });
    drop.addEventListener('click', () => fileIn.click());
    drop.addEventListener('keydown', (e) => { if (e.target === drop && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); fileIn.click(); } });
    drop.addEventListener('paste', (e) => {
      const text = e.clipboardData && e.clipboardData.getData('text');
      if (text && text.trim()) { e.preventDefault(); wizard(text); }
    });
    wireDrop(drop, fromFile);
    replace(heroEl, h('div', null, drop, fileIn));
  }

  function matches(m) {
    if (st.filter === 'on' && !m.enabled) return false;
    if (st.filter === 'off' && m.enabled) return false;
    if (st.filter === 'secured' && !m.security.enabled) return false;
    if (st.filter === 'draft' && !m.has_draft_changes && m.live_revision !== null) return false;
    if (!st.q) return true;
    return (m.name + ' ' + m.title + ' ' + m.url).toLowerCase().includes(st.q);
  }

  function renderBulk(list) {
    const visible = list.filter(canChange).map((m) => m.name);
    // Only mocks the caller may change can be selected.
    if (!visible.length) { bulkEl.remove(); return; }
    if (!bulkEl.isConnected) listEl.before(bulkEl);
    const chosen = visible.filter((n) => selected.has(n));
    const all = h('input', { type: 'checkbox', 'aria-label': 'Select every mock shown', checked: visible.length > 0 && chosen.length === visible.length });
    all.indeterminate = chosen.length > 0 && chosen.length < visible.length;
    all.addEventListener('change', () => {
      for (const n of visible) { if (all.checked) selected.add(n); else selected.delete(n); }
      renderList();
    });
    const n = selected.size;
    replace(bulkEl, h('label', { class: 'check small' }, all, n ? plural(n, 'mock') + ' selected' : 'Select'),
      n ? h('span', { class: 'row', style: { gap: '6px' } },
        h('button', { class: 'btn sm', type: 'button', onclick: () => bulk('enable') }, 'Turn on'),
        h('button', { class: 'btn sm', type: 'button', onclick: () => bulk('disable') }, 'Turn off'),
        h('button', { class: 'btn sm danger', type: 'button', onclick: () => bulk('delete') }, icon('trash'), 'Delete'),
        h('button', { class: 'btn sm ghost', type: 'button', onclick: () => { selected.clear(); renderList(); } }, 'Clear selection')) : null);
    bulkEl.classList.toggle('active', n > 0);
  }

  async function bulk(action) {
    const names = Array.from(selected);
    if (!names.length) return;
    if (action === 'delete' && !confirm(`Delete ${plural(names.length, 'mock')} (${names.join(', ')})? Their URLs stop answering and their revisions and traffic are lost.`)) return;
    const failed = [];
    for (const n of names) {
      const r = action === 'delete' ? await api('DELETE', '/' + encodeURIComponent(n)) : await api('POST', `/${encodeURIComponent(n)}/${action}`);
      if (!r.ok) failed.push(`${n}: ${errorText(r)}`);
      else if (action === 'delete') selected.delete(n);
    }
    const done = names.length - failed.length;
    const verb = { enable: 'switched on', disable: 'switched off', delete: 'deleted' }[action];
    if (failed.length) toast(`${done} of ${names.length} ${verb}. ${failed.join('; ')}`, 'err');
    else toast(`${plural(done, 'mock')} ${verb}`);
    load();
  }

  function renderList() {
    if (!data) return;
    for (const n of Array.from(selected)) if (!data.mocks.some((m) => m.name === n)) selected.delete(n);
    const list = data.mocks.filter(matches);
    renderBulk(list);
    clear(listEl);
    if (!data.mocks.length) {
      listEl.appendChild(empty('No mocks yet', readOnly && !visitor ? 'Nobody created a mock on this instance yet.' : 'Create one from an OpenAPI spec with "New mock".', 'mock'));
      return;
    }
    if (!list.length) {
      listEl.appendChild(empty('No mock matches', 'Try another search or filter.'));
      return;
    }
    const group = (items) => {
      const rows = h('div', { class: 'mock-list' + (items.some(canChange) ? ' selectable' : ''), role: 'list' });
      for (const m of items) rows.appendChild(row(m));
      return rows;
    };
    if (!data.self_service || !data.self_service.enabled) {
      listEl.appendChild(group(list));
      return;
    }
    const yours = list.filter((m) => m.yours);
    const others = list.filter((m) => !m.yours);
    if (yours.length) listEl.append(h('h3', { class: 'label mock-group' }, 'Your mocks'), group(yours));
    if (others.length) listEl.append(h('h3', { class: 'label mock-group' }, visitor ? 'Instance mocks (read-only)' : 'Instance mocks'), group(others));
  }

  function row(m) {
    const sw = toggleSwitch(`${m.name} on or off`, m.enabled, async (on) => {
      const r = await api('POST', `/${encodeURIComponent(m.name)}/${on ? 'enable' : 'disable'}`);
      if (report(r, `${m.name} is ${on ? 'on' : 'off'}`, 'Switching')) { m.enabled = on; load(); } else sw.set(!on);
    }, { disabled: !canChange(m) });
    const check = h('input', { type: 'checkbox', 'aria-label': 'Select ' + m.name, checked: selected.has(m.name) });
    check.addEventListener('change', () => { if (check.checked) selected.add(m.name); else selected.delete(m.name); renderList(); });
    const open = () => { location.hash = '#/mocks/' + encodeURIComponent(m.name); };
    const el = h('div', { class: 'mock-row', role: 'listitem', 'data-mock': m.name },
      h('div', { class: 'mr-switch' }, canChange(m) ? check : null, sw.el),
      h('div', { class: 'mr-name' },
        h('div', { class: 'row nowrap', style: { gap: '6px' } },
          h('a', { class: 'mono mr-link', href: '#/mocks/' + encodeURIComponent(m.name) }, m.name),
          m.has_draft_changes && !m.load_error ? h('span', { class: 'dot warn', title: 'Draft changes not deployed', 'aria-label': 'draft changes' }) : null,
          m.load_error ? badge(h('span', { title: m.load_error }, icon('alert'), ' load error'), 'err') : null,
          m.origin === 'sample' ? badge('sample', 'outline') : m.origin === 'file' ? badge('file', 'outline') : null,
          m.owner_fingerprint ? badge(h('span', { title: 'Visitor mock (owner ' + m.owner_fingerprint + ')' }, 'visitor'), 'outline') : null),
        h('div', { class: 'row nowrap', style: { gap: '4px' } }, h('code', { class: 'tiny muted ellipsis' }, m.url), copyButton(m.url, { icon: true, title: 'Copy the URL of ' + m.name, toast: 'Mock URL copied' })),
        m.host_url ? h('div', { class: 'row nowrap', style: { gap: '4px' } }, h('code', { class: 'tiny muted ellipsis', title: 'Host-based URL' }, m.host_url), copyButton(m.host_url, { icon: true, title: 'Copy the host URL of ' + m.name, toast: 'Host URL copied' })) : null),
      h('div', { class: 'mr-title' }, h('div', { class: 'ellipsis' }, m.title), h('div', { class: 'tiny muted' }, 'v' + m.version + ' | ' + m.operations + (m.operations === 1 ? ' operation' : ' operations') + (m.mode === 'stateful' ? ' | stateful, ' + plural(m.state_items || 0, 'item') + ' stored' : ''))),
      h('div', { class: 'mr-sec' }, m.security.enabled ? badge(h('span', null, icon('lock'), ' ' + securityLabel(m.security)), 'violet') : badge('open', '')),
      h('div', { class: 'mr-hits small' }, h('div', null, h('strong', null, String(m.requests_last_hour || 0)), ' in the last hour'),
        h('div', { class: 'tiny muted', title: m.last_hit || '' }, plural(m.requests_total, 'call') + ' in total' + (m.last_hit ? ', last ' + relTime(m.last_hit) : ''))),
      h('div', { class: 'mr-state' }, statePill(m)));
    el.addEventListener('click', (e) => { if (!e.target.closest('button, a, input')) open(); });
    return el;
  }

  async function importYaml(file) {
    let text;
    try { text = await readFile(file, 4 * 1024 * 1024); } catch (e) { toast(e.message, 'err'); return; }
    const r = await api('POST', '/apply', text, { contentType: 'application/yaml', timeout: 60000 });
    if (!r.ok || !r.json) { toast(errorText(r, 'Importing'), 'err'); return; }
    const rep = r.json;
    const parts = [['created', rep.created], ['updated', rep.updated], ['unchanged', rep.unchanged], ['deleted', rep.deleted]].filter(([, l]) => l && l.length).map(([k, l]) => `${k} ${l.join(', ')}`);
    toast('Imported: ' + (parts.join('; ') || 'nothing to do'));
    if ((rep.errors || []).length) {
      noticeEl.appendChild(notice('err', h('strong', null, 'Some mocks of ' + file.name + ' were not applied:'),
        h('ul', { class: 'issue-list small' }, ...rep.errors.map((e) => h('li', null, (e.name ? e.name + ': ' : '') + (e.error || JSON.stringify(e)))))));
    }
    load();
  }

  async function exportAll() {
    const r = await api('GET', '/export');
    if (!r.ok) { toast(errorText(r, 'Export'), 'err'); return; }
    if (data && !data.mocks.some((m) => m.origin !== 'sample')) toast('No mocks of your own yet: the file is empty (the sample is not exported).');
    download('rustybin.yaml', r.text, 'application/yaml');
  }

  load();
  // Calls and "last hit" change all the time: refresh quietly.
  timer = setInterval(() => { if (!document.hidden && !document.querySelector('dialog[open]') && !listEl.contains(document.activeElement)) load(); }, 5000);
  return { destroy() { alive = false; clearInterval(timer); } };
}

export default {
  id: 'mocks',
  mount(root, ctx) {
    let current = null;
    let currentName = null;
    function show(rest) {
      const [name, ...sub] = (rest || '').split('/');
      if (current && name && name === currentName && current.route) {
        current.route(sub.join('/'));
        return;
      }
      if (current) current.destroy();
      clear(root);
      currentName = name || null;
      current = name ? mountDetail(root, name, sub.join('/')) : mountList(root);
      document.getElementById('main').scrollTop = 0;
    }
    show(ctx && ctx.rest);
    return {
      destroy() { if (current) current.destroy(); current = null; },
      onRoute: show,
    };
  },
};
