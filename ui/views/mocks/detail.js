// Detail of one mock (#/mocks/{name}[/{tab}[/{operation}]]): header with the
// URL, on/off switch, Deploy and more actions, then tabs for operations (with
// the response editor), Try it, Security, the mock's MCP server, Settings,
// Traffic and the deploy report with revision history.
import { h, replace, clear, card, icon, notice, spinner, empty, methodBadge, badge, input, select, textarea, toast, copyButton, copyText, toggleSwitch, tabs, kvgrid, debounce } from '../../lib/dom.js';
import { copyableCode } from '../../lib/format.js';
import { toCurl, trimBase } from '../../lib/http.js';
import { jsonView } from '../../lib/format.js';
import { api, errorText, report, readOnlyReason, mockReadOnly, setMockBase, nameProblem, statePill, securityLabel, relTime, groupByTag, download, readFile, sampleToRequest, MODES, COLLECTIONS, isStateful, plural, dispositionName } from '../../lib/mockapi.js';
import * as visit from '../../lib/visit.js';
import { operationEditor, checkIcon, responseChecks } from './operation.js';
import { tryPanel, securityPanel, trafficPanel, mcpPanel } from './panels.js';
import { checksFailed, wireDrop } from './wizard.js';
import { coverageBanner, openEditDialog, openExamplesDialog } from './ai.js';
import * as assist from '../../lib/assist.js';
import * as settings from '../../lib/settings.js';

const enc = encodeURIComponent;
const TABS = [['operations', 'Operations'], ['try', 'Try it'], ['data', 'Data'], ['security', 'Security'], ['mcp', 'MCP server'], ['settings', 'Settings'], ['traffic', 'Traffic'], ['report', 'Deploy report']];
const SOURCE_LABEL = { examples: 'Example', generated: 'Generated', static: 'Static', stateful: 'Stateful', override: 'Override', custom: 'Custom', rules: 'Rules' };

/** Mount the detail of mock `name` into `root`. Returns {destroy, route(sub)}. */
export function mountDetail(root, name, sub) {
  let alive = true;
  let detail = null;
  let tab = 'operations';
  let opId = null;
  let editor = null;
  let pollTimer = null;
  // Decided per mock once it is loaded (a visitor may change their own).
  let readOnly = readOnlyReason();
  const headEl = h('div', { class: 'view-head mock-head' }, spinner('Loading the mock'));
  // Banners: the one-off "created" message, the state of the draft (rebuilt
  // on every refresh) and the result of a failed deploy.
  const createdEl = h('div');
  const stateEl = h('div', { class: 'stack-sm' });
  const deployFailEl = h('div');
  const coverageEl = h('div');
  // What the AI dialogs need of this page.
  // After an AI dialog saved to the draft, the open operation editor re-reads its operation too.
  const aiCtx = { name, detail: () => detail, refresh: async () => { await refresh(); if (editor && editor.reload) editor.reload(); }, get readOnly() { return !!readOnly; } };
  const bannerEl = h('div', { class: 'stack-sm mock-banners' }, createdEl, deployFailEl, stateEl, coverageEl);
  const bodyEl = h('div');
  root.append(headEl, bannerEl, bodyEl);
  const closeMenu = (e) => { if (menu.open && !menu.contains(e.target)) menu.open = false; };
  document.addEventListener('click', closeMenu);

  // ── Data ──
  async function refresh(headerOnly) {
    const r = await api('GET', '/' + enc(name));
    if (!alive) return;
    if (r.status === 404) {
      replace(headEl);
      replace(bannerEl);
      replace(bodyEl, card(null, null, empty('This mock does not exist', 'It may have been deleted or renamed.', 'mock'), h('div', { class: 'row', style: { justifyContent: 'center' } }, h('a', { class: 'btn', href: '#/mocks' }, icon('back'), 'All mocks'))));
      detail = null;
      return;
    }
    if (!r.ok || !r.json) {
      if (!detail) replace(bodyEl, notice('err', errorText(r, 'Loading the mock'), ' ', h('button', { class: 'btn sm', type: 'button', onclick: () => refresh() }, 'Retry')));
      return;
    }
    const first = !detail;
    // A new draft makes an earlier "deploy failed" message stale.
    if (detail && detail.draft_revision !== r.json.draft_revision) replace(deployFailEl);
    detail = r.json;
    setMockBase(name, detail.base_url);
    if (first) {
      readOnly = mockReadOnly(detail);
      switchCtl.el.disabled = !!readOnly;
    }
    renderHead();
    renderBanner();
    if (first) {
      buildTabs();
      // Apply cards of the assistant land here with a request.
      const edit = settings.recall('mocks.edit', null);
      if (edit && edit.mock === name && !readOnly) { settings.remember('mocks.edit', null); setTimeout(() => openEditDialog(aiCtx, edit.instruction), 0); }
      const gen = settings.recall('mocks.generate', null);
      if (gen && gen.mock === name && !readOnly) { settings.remember('mocks.generate', null); setTimeout(() => openExamplesDialog(aiCtx, assist.enabled() ? 'ai' : 'generated'), 0); }
    } else {
      // The Data tab exists while the mock (or an operation) is stateful.
      if (isStateful(detail) !== hasData) buildTabControl();
      if (!headerOnly) for (const p of Object.values(panels)) if (p && p.update) p.update(detail);
      else if (panels.data && tab === 'data') panels.data.update(detail);
    }
  }

  // ── Header ──
  const switchCtl = toggleSwitch('Mock on or off', false, async (on) => {
    const r = await api('POST', `/${enc(name)}/${on ? 'enable' : 'disable'}`);
    if (report(r, `${name} is ${on ? 'on' : 'off'}`, 'Switching')) refresh(true);
    else switchCtl.set(!on);
  }, { disabled: !!readOnly });
  const switchText = h('strong', { class: 'small' });
  const deployBtn = h('button', { class: 'btn', type: 'button' });
  deployBtn.addEventListener('click', () => deploy(false));
  const menu = h('details', { class: 'menu' });

  function renderHead() {
    const d = detail;
    switchCtl.set(d.enabled);
    switchText.textContent = d.enabled ? 'On' : 'Off';
    // A file mock that never loaded has nothing to deploy: fix the file.
    const unloaded = !!d.load_error && d.live_revision === null;
    const pending = !unloaded && (d.has_draft_changes || d.live_revision === null);
    deployBtn.className = 'btn' + (pending && !readOnly ? ' primary' : '');
    deployBtn.disabled = !pending || !!readOnly;
    const c = d.report;
    const changes = d.draft_changes || [];
    const counts = unloaded ? null : c.errors ? badge(String(c.errors), 'err') : c.warnings ? badge(String(c.warnings), 'warn') : null;
    replace(deployBtn, icon('up'), 'Deploy', changes.length ? h('span', { class: 'tiny' }, ` (${plural(changes.length, 'change')})`) : null, counts);
    deployBtn.title = unloaded ? 'Nothing to deploy: fix the definition in the file, then reload.' : pending ? `Publish the draft (revision ${d.draft_revision})${changes.length ? '. Changed: ' + changes.join(', ') : ''}. Checks: ${c.errors} errors, ${c.warnings} warnings.` : 'Nothing to deploy: the draft is live.';
    buildMenu();
    replace(headEl,
      h('div', { class: 'grow stack-sm', style: { gap: '6px' } },
        h('a', { class: 'small back-link', href: '#/mocks' }, icon('back'), 'All mocks'),
        h('div', { class: 'row' }, h('h1', { class: 'mono' }, d.name), statePill(d),
          d.has_draft_changes && !unloaded ? badge(h('span', null, h('span', { class: 'dot warn' }), ' draft changes'), 'warn') : null,
          d.origin !== 'api' ? badge(d.origin === 'sample' ? 'built-in sample' : 'from file', 'outline') : null),
        h('p', { style: { margin: 0 } }, h('strong', null, d.spec.title), ' v', d.spec.version, h('span', { class: 'muted' }, ` | ${plural(d.operations.length, 'operation')} | security ${securityLabel(d.security)} | ${plural(d.requests_last_hour || 0, 'call')} in the last hour, ${d.requests_total} in total, last ${relTime(d.last_hit)}`)),
        h('div', { class: 'row nowrap url-bar' }, h('code', { class: 'grow break' }, d.url), copyButton(d.url, { label: 'Copy URL', toast: 'Mock URL copied' })),
        d.host_url ? h('div', { class: 'row nowrap url-bar' }, h('span', { class: 'tiny muted' }, 'Host'), h('code', { class: 'grow break' }, d.host_url), copyButton(d.host_url, { label: 'Copy', title: 'Copy the host-based URL', toast: 'Host URL copied' })) : null),
      h('div', { class: 'row mock-actions' },
        h('span', { class: 'row nowrap', style: { gap: '8px' } }, switchCtl.el, switchText),
        readOnly ? null : editAiButton(),
        readOnly ? null : deployBtn, menu));
  }

  function editAiButton() {
    const b = assist.assistButton('Edit with AI', 'edit_spec', () => ({}), { class: '' });
    b.id = 'edit-with-ai';
    b.onclick = (e) => { e.stopImmediatePropagation(); if (!assist.enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; } openEditDialog(aiCtx); };
    return b;
  }

  function buildMenu() {
    const d = detail;
    const item = (label, ic, fn, cls) => h('button', { type: 'button', role: 'menuitem', class: cls || '', onclick: () => { menu.open = false; fn(); } }, icon(ic), label);
    replace(menu,
      h('summary', { class: 'btn icon', 'aria-label': 'More actions', title: 'More actions' }, icon('more')),
      h('div', { class: 'menu-pop', role: 'menu' },
        readOnly || d.yours ? null : item('Rename', 'edit', () => goTab('settings', null, '#mock-rename')),
        readOnly ? null : item('Duplicate', 'copy', duplicateMock),
        readOnly ? null : item('Generate examples', 'ai', () => { if (!assist.enabled()) { location.hash = '#/manage/ai'; return; } openExamplesDialog(aiCtx, 'ai', detail.operations.map((o) => o.id)); }),
        readOnly ? null : item('Use generated values', 'refresh', () => openExamplesDialog(aiCtx, 'generated', detail.operations.map((o) => o.id))),
        item('Export YAML', 'download', exportYaml),
        item('Download spec', 'file', () => downloadFrom('/spec', name + '.openapi.yaml')),
        item('Download collection', 'download', collectionDialog),
        d.live_revision !== null ? h('a', { role: 'menuitem', href: d.spec.spec_url, target: '_blank', rel: 'noopener', onclick: () => { menu.open = false; } }, icon('external'), 'Open served spec') : null,
        readOnly ? null : item('Rollback', 'history', () => goTab('report', null, '#mock-revisions')),
        readOnly ? null : item('Clear traffic', 'trash', clearTraffic),
        d.yours && visit.tokens()[0] ? item('Copy manage link', 'link', () => copyText(visit.manageLink(visit.tokens()[0]), 'Manage link copied: open it in another browser to manage your mocks there')) : null,
        d.yours ? item('Rotate manage token', 'refresh', rotateToken) : null,
        readOnly ? null : item('Delete', 'trash', deleteMock, 'danger')));
  }

  async function rotateToken() {
    if (!confirm('Replace your manage token? Manage links you shared stop working; this browser keeps access.')) return;
    const old = visit.tokens()[0];
    const r = await api('POST', '/visit/token');
    if (r.ok && r.json && r.json.manage_token) {
      visit.replaceToken(old, r.json.manage_token);
      toast('New manage token saved in this browser');
    } else {
      toast(errorText(r, 'Rotating the token'), 'err');
    }
  }

  function renderBanner() {
    clear(stateEl);
    const created = settings.recall('mocks.created', null);
    if (created && created.name === name) {
      settings.remember('mocks.created', null);
      replace(createdEl, createdBanner(created.deployed));
    }
    if (readOnly) stateEl.appendChild(notice('', readOnly));
    if (detail.load_error) {
      stateEl.appendChild(notice('err', h('strong', null, 'The last reload of this mock failed. '),
        detail.live_revision === null ? 'It was never loaded, so its URL answers 503. ' : `Revision ${detail.live_revision} keeps answering. `,
        'Fix the definition in the file, then reload (SIGHUP or POST /_rustybin/mocks/reload).',
        h('pre', { class: 'tiny break', style: { margin: '6px 0 0', whiteSpace: 'pre-wrap' } }, detail.load_error)));
    } else if (detail.live_revision === null) {
      stateEl.appendChild(notice('warn', h('strong', null, 'Not deployed yet. '), 'Calls to its URL answer 503 until you deploy the draft.', readOnly ? null : h('div', { style: { marginTop: '6px' } }, h('button', { class: 'btn sm primary', type: 'button', onclick: () => deploy(false) }, icon('up'), 'Deploy now'))));
    } else if (detail.has_draft_changes) {
      const changes = detail.draft_changes || [];
      stateEl.appendChild(notice('warn', h('strong', null, 'The draft has changes that are not live yet. '), `Revision ${detail.live_revision} answers calls; deploy to publish revision ${detail.draft_revision}.`,
        changes.length ? h('div', { class: 'small', style: { marginTop: '4px' } }, 'Changed: ', changes.join(', ')) : null,
        readOnly ? null : h('div', { style: { marginTop: '6px' } }, h('button', { class: 'btn sm primary', type: 'button', onclick: () => deploy(false) }, icon('up'), 'Deploy'))));
    }
    if (!detail.enabled && !(detail.load_error && detail.live_revision === null)) stateEl.appendChild(notice('', 'This mock is switched off: its URL answers ' + detail.settings.responses.disabled_status + '. Configuration and history are kept.'));
    replace(coverageEl, coverageBanner(aiCtx));
  }

  function createdBanner(deployed) {
    const host = h('div', { class: 'stack-sm' }, h('div', { class: 'row nowrap' }, h('code', { class: 'grow break' }, detail.url), copyButton(detail.url, { label: 'Copy URL' })));
    const el = notice('ok', h('strong', null, deployed ? 'Your mock is live. ' : 'Draft created. '),
      deployed ? 'Call it from anywhere, for example with this curl:' : 'Deploy it when you are ready; its URL answers 503 until then.', host,
      detail.yours && visit.tokens()[0] ? h('div', { class: 'small', style: { marginTop: '6px' } },
        'Only this browser can change it: its manage token is saved here. To manage it from another browser, ',
        copyButton(() => visit.manageLink(visit.tokens()[0]), { label: 'Copy the manage link', toast: 'Manage link copied' }), '.') : null);
    const op = detail.operations.find((o) => o.method === 'GET') || detail.operations[0];
    if (op && deployed) {
      api('GET', `/${enc(name)}/operations/${enc(op.id)}`).then((r) => {
        if (!r.ok || !r.json || !alive) return;
        const req = sampleToRequest(name, r.json.sample_request);
        host.appendChild(copyableCode(toCurl({ method: req.method, url: req.url, headers: req.headers, body: req.body })));
      });
    }
    return el;
  }

  // ── Actions ──
  async function deploy(force) {
    deployBtn.disabled = true;
    const r = await api('POST', `/${enc(name)}/deploy${force ? '?force=true' : ''}`, undefined, { timeout: 60000 });
    if (!alive) return;
    if (r.ok && r.json) {
      replace(deployFailEl);
      toast(`Deployed revision ${r.json.revision}` + (r.json.report && r.json.report.warnings ? ` (${r.json.report.warnings} warnings)` : ''));
      await refresh();
      return;
    }
    deployBtn.disabled = false;
    if (r.status === 422 && r.json && r.json.report) {
      replace(deployFailEl, h('div', { class: 'stack-sm' }, checksFailed(r.json.report, () => deploy(true), null),
        h('div', { class: 'row' }, h('button', { class: 'btn sm', type: 'button', onclick: () => goTab('report') }, 'Open the deploy report'))));
      return;
    }
    toast(errorText(r, 'Deploying'), 'err');
  }

  async function exportYaml() {
    const r = await api('GET', `/${enc(name)}/export`);
    if (r.ok) download(name + '.rustybin.yaml', r.text, 'application/yaml');
    else toast(errorText(r, 'Export'), 'err');
  }

  async function downloadFrom(path, fallback) {
    const r = await api('GET', `/${enc(name)}${path}`, undefined, { timeout: 30000 });
    if (!r.ok) { toast(errorText(r, 'Download'), 'err'); return false; }
    download(dispositionName(r, fallback), r.text, (r.contentType || 'text/plain').split(';')[0]);
    return true;
  }

  /** Small modal dialog: {dlg, body, foot}; removed from the page when closed. */
  function modal(title, id) {
    const body = h('div', { class: 'wizard-body stack' });
    const foot = h('div', { class: 'wizard-foot row' });
    const dlg = h('dialog', { class: 'wizard small-dialog', 'aria-labelledby': id },
      h('div', { class: 'wizard-head row nowrap' }, h('h2', { id, class: 'grow' }, title),
        h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', title: 'Close (Esc)', onclick: () => dlg.close() }, icon('x'))),
      body, foot);
    dlg.addEventListener('close', () => dlg.remove());
    document.body.appendChild(dlg);
    dlg.showModal();
    return { dlg, body, foot };
  }

  function duplicateMock() {
    const { dlg, body, foot } = modal('Duplicate ' + name, 'dup-title');
    const nameIn = input({ value: (name.length > 35 ? name.slice(0, 35).replace(/-+$/, '') : name) + '-copy', class: 'input mono', 'aria-label': 'Name of the copy', maxlength: '40' });
    const msg = h('span', { class: 'hint' }, 'The copy gets the same spec and settings, starts switched on (deployed when this mock is) with empty traffic.');
    const err = h('div');
    const go = h('button', { class: 'btn primary', type: 'button' }, icon('copy'), 'Duplicate');
    async function run() {
      const n = nameIn.value.trim();
      const why = n ? nameProblem(n) : '';
      if (why) { replace(err, notice('err', why)); return; }
      go.disabled = true;
      const r = await api('POST', `/${enc(name)}/duplicate`, n ? { name: n } : {});
      go.disabled = false;
      if (r.status === 201 && r.json) {
        dlg.close();
        toast(`Duplicated as ${r.json.name}`);
        location.hash = '#/mocks/' + enc(r.json.name);
      } else replace(err, notice('err', errorText(r, 'Duplicating')));
    }
    go.addEventListener('click', run);
    nameIn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); run(); } });
    replace(body, h('label', { class: 'field' }, h('span', null, 'Name of the copy'), nameIn, msg), err);
    replace(foot, h('span', { class: 'grow' }), h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'), go);
    nameIn.focus();
    nameIn.select();
  }

  function collectionDialog() {
    const { dlg, body, foot } = modal('Download a collection', 'coll-title');
    let format = settings.recall('mocks.collection', 'postman');
    const cards = h('div', { class: 'radio-cards', role: 'radiogroup', 'aria-label': 'Format' });
    for (const [value, label, text] of COLLECTIONS) {
      const radio = h('input', { type: 'radio', name: 'coll-format', value, checked: value === format });
      radio.addEventListener('change', () => { format = value; });
      cards.appendChild(h('label', { class: 'radio-card' }, radio, h('span', null, h('strong', null, label), h('span', { class: 'small muted' }, ' ' + text))));
    }
    const baseIn = input({ class: 'input mono', placeholder: detail.base_url, 'aria-label': 'Base URL', type: 'url' });
    const gw = trimBase(settings.get('gatewayUrl'));
    const err = h('div');
    const go = h('button', { class: 'btn primary', type: 'button' }, icon('download'), 'Download');
    go.addEventListener('click', async () => {
      const base = trimBase(baseIn.value);
      if (base && !/^https?:\/\/\S+$/.test(base)) { replace(err, notice('err', 'The base URL must start with http:// or https://')); return; }
      settings.remember('mocks.collection', format);
      go.disabled = true;
      const ok = await downloadFrom(`/collections/${format}${base ? '?base_url=' + enc(base) : ''}`, name + '.' + format);
      go.disabled = false;
      if (ok) { dlg.close(); toast('Collection downloaded'); }
    });
    replace(body, cards,
      h('div', { class: 'field' }, h('span', null, 'Base URL'), baseIn,
        h('span', { class: 'hint' }, 'Empty: the mock itself (', h('code', null, detail.base_url), '). Point it at your gateway route to run the requests through the gateway.'),
        gw ? h('div', null, h('button', { class: 'btn sm', type: 'button', onclick: () => { baseIn.value = gw + '/mock/' + name; } }, 'Use the gateway URL from the settings')) : null),
      err);
    replace(foot, h('span', { class: 'grow' }), h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'), go);
  }

  async function clearTraffic() {
    if (!confirm(`Clear every captured call of ${name}?`)) return;
    const r = await api('DELETE', `/${enc(name)}/requests`);
    if (report(r, 'Traffic cleared', 'Clearing')) {
      if (panels.traffic) panels.traffic.clearLocal();
      refresh(true);
    }
  }

  async function deleteMock() {
    if (!confirm(`Delete the mock "${name}"? Its URL stops answering and its revisions and traffic are lost.`)) return;
    const r = await api('DELETE', '/' + enc(name));
    if (report(r, `Deleted ${name}`, 'Deleting')) location.hash = '#/mocks';
  }

  // ── Tabs ──
  const panels = {};
  let tabCtl = null;

  function setHash() {
    const h2 = '#/mocks/' + enc(name) + (tab !== 'operations' || opId ? '/' + tab : '') + (tab === 'operations' && opId ? '/' + enc(opId) : '');
    if (location.hash !== h2) history.replaceState(history.state, '', h2);
  }

  function goTab(id, op, focusSel) {
    if (op) opId = op;
    tabCtl.select(id);
    if (focusSel) setTimeout(() => { const f = bodyEl.querySelector(focusSel); if (f) { f.scrollIntoView({ block: 'center' }); if (f.focus) f.focus(); } }, 50);
  }

  function buildTabs() {
    const ctx = { name, detail: () => detail, refresh: () => refresh(), refreshHead: () => refresh(true), readOnly: !!readOnly, goTab: (id) => goTab(id) };
    panels.operations = operationsPanel();
    panels.try = tryPanel(ctx);
    panels.security = securityPanel(ctx);
    panels.mcp = mcpPanel(ctx);
    panels.settings = settingsPanel();
    panels.traffic = trafficPanel(ctx);
    panels.report = reportPanel();
    panels.data = dataPanel();
    for (const p of Object.values(panels)) if (p.update) p.update(detail);
    buildTabControl();
  }

  let hasData = false;
  function buildTabControl() {
    hasData = isStateful(detail);
    const items = TABS.filter(([id]) => id !== 'data' || hasData);
    if (!items.some(([id]) => id === tab)) tab = 'operations';
    tabCtl = tabs(items.map(([id, label]) => ({ id, label, render: () => panels[id].el })), tab, (id) => {
      tab = id;
      if (panels[id].show) panels[id].show();
      setHash();
    });
    tabCtl.el.classList.add('mock-tabs');
    replace(bodyEl, tabCtl.el);
  }

  function route(rest) {
    const parts = (rest || '').split('/');
    const t = TABS.some(([id]) => id === parts[0]) && (parts[0] !== 'data' || !detail || hasData) ? parts[0] : 'operations';
    tab = t;
    if (t === 'operations' && parts[1]) opId = parts[1];
    if (tabCtl) {
      tabCtl.select(t);
      if (t === 'operations' && opId && panels.operations) panels.operations.open(opId);
    }
  }

  // ── Operations ──
  function operationsPanel() {
    const listEl = h('div', { class: 'list', role: 'listbox', 'aria-label': 'Operations' });
    const filterIn = input({ type: 'search', placeholder: 'Filter operations', 'aria-label': 'Filter operations' });
    const editorHost = h('div', { class: 'op-editor' });
    filterIn.addEventListener('input', debounce(renderList, 100));
    const el = h('div', { class: 'ops-layout' },
      h('div', { class: 'card ops-list' }, h('div', { class: 'card-body', style: { borderBottom: '1px solid var(--border)' } }, filterIn), h('div', { class: 'pane-scroll' }, listEl)),
      editorHost);

    function renderList() {
      clear(listEl);
      const q = filterIn.value.trim().toLowerCase();
      const ops = detail.operations.filter((o) => !q || (o.method + ' ' + o.path + ' ' + (o.summary || '') + ' ' + o.id).toLowerCase().includes(q));
      for (const [tag, list] of groupByTag(ops)) {
        listEl.appendChild(h('div', { class: 'nav-section', style: { padding: '10px 12px 4px' } }, tag));
        for (const op of list) {
          listEl.appendChild(h('button', { class: 'list-item op-row', type: 'button', role: 'option', 'aria-selected': String(op.id === opId), onclick: () => open(op.id) },
            methodBadge(op.method),
            h('span', { style: { minWidth: 0 } }, h('div', { class: 'mono ellipsis', style: { fontWeight: 600 } }, op.path),
              h('div', { class: 'tiny muted ellipsis' }, op.summary || op.id, op.hits ? h('span', { class: 'op-hits', title: op.last_hit ? 'last call ' + relTime(op.last_hit) : '' }, ' | ' + plural(op.hits, 'hit')) : null)),
            h('span', { class: 'row nowrap', style: { gap: '4px' } },
              badge(SOURCE_LABEL[op.source] + (op.source === 'rules' ? ' ' + op.rules : ''), op.source === 'examples' ? '' : 'info'),
              op.secured ? h('span', { class: 'lock', title: 'Secured' }, icon('lock')) : null,
              checkIcon(op.check))));
        }
      }
      if (!ops.length) listEl.appendChild(empty('No operation matches', q ? 'Try another filter.' : 'The spec has no operations.'));
    }

    function open(id) {
      if (editor && id === opId) { editor.refreshPreview(); return; }
      if (editor && editor.dirty() && id !== opId && !confirm('Discard the unsaved changes of this operation?')) return;
      if (!detail.operations.some((o) => o.id === id)) return;
      if (editor) editor.destroy();
      opId = id;
      renderList();
      editor = operationEditor({
        name, opId: id, readOnly: !!readOnly, mockMode: detail.settings.responses.mode,
        visitor: detail.owner === 'visit' || detail.owner === 'key',
        securityOn: detail.security.enabled, maxDelayMs: detail.limits && detail.limits.max_delay_ms,
        onSaved: () => refresh(),
        onGenerate: (op) => openExamplesDialog(aiCtx, assist.enabled() ? 'ai' : 'generated', [op]),
        onTry: (op) => { goTab('try'); panels.try.select(op); },
        onData: () => { if (hasData) goTab('data'); else toast('Save first: the Data tab appears once the mock or an operation is stateful.'); },
      });
      replace(editorHost, editor.el);
      setHash();
      if (window.matchMedia && window.matchMedia('(max-width: 1100px)').matches) editorHost.scrollIntoView({ block: 'start' });
    }

    return {
      el,
      open,
      update() {
        renderList();
        if (!editor) {
          // The requested operation, else the first one (nothing to pick from an empty editor).
          const want = opId && detail.operations.some((o) => o.id === opId) ? opId : detail.operations.length ? detail.operations[0].id : null;
          if (want) open(want);
          else replace(editorHost, card(null, null, empty('Pick an operation', 'Choose the response it sends (spec example, generated data or a custom body built from the schema), add rules for specific requests, then deploy.', 'edit')));
        }
      },
      show() { if (editor) editor.refreshPreview(); },
    };
  }

  // ── Settings ──
  function settingsPanel() {
    const el = h('div', { class: 'stack' });
    let dirty = false;
    function render() {
      dirty = false;
      const s = detail.settings;
      const r = s.responses;
      const visitorMock = detail.owner === 'visit' || detail.owner === 'key';
      const mode = select(MODES.filter(([v]) => !visitorMock || v !== 'stateful').map(([v, l]) => [v, l]), r.mode, { 'aria-label': 'Response mode' });
      const modeHint = h('span', { class: 'hint' });
      const showModeHint = () => { modeHint.textContent = ((MODES.find(([v]) => v === mode.value) || [])[2] || '') + ' Per operation overrides win.'; };
      mode.addEventListener('change', showModeHint);
      showModeHint();
      const seed = input({ type: 'number', min: '0', value: String(r.seed), class: 'input mono', 'aria-label': 'Seed' });
      const delay = input({ type: 'number', min: '0', value: String(r.delay_ms), class: 'input mono', 'aria-label': 'Delay (ms)' });
      const validate = select([['off', 'Off'], ['warn', 'Warn (answer, flag problems)'], ['enforce', 'Enforce (answer 400)']], r.validate_requests, { 'aria-label': 'Validate requests' });
      const disabled = select([['503', '503 Service Unavailable'], ['404', '404 Not Found']], String(r.disabled_status), { 'aria-label': 'Status while switched off' });
      const strip = h('input', { type: 'checkbox', checked: s.strip_server_prefix });
      const saveBtn = h('button', { class: 'btn primary', type: 'button' }, 'Save to draft');
      const form = h('fieldset', { class: 'plain stack-sm', disabled: !!readOnly },
        h('div', { class: 'grid grid-2' },
          h('label', { class: 'field' }, h('span', null, 'Responses'), mode, modeHint),
          h('label', { class: 'field' }, h('span', null, 'Validate requests'), validate, h('span', { class: 'hint' }, 'Against parameters and request bodies of the spec.')),
          h('label', { class: 'field' }, h('span', null, 'Seed of generated data'), seed, h('span', { class: 'hint' }, 'Same seed, same data. Change it for a fresh data set.')),
          h('label', { class: 'field' }, h('span', null, 'Delay for every call (ms)'), delay, h('span', { class: 'hint' }, `At most ${detail.limits.max_delay_ms} ms.`)),
          h('label', { class: 'field' }, h('span', null, 'Status while switched off'), disabled)),
        detail.spec.server_prefix ? h('label', { class: 'check' }, strip, 'Serve the paths without the spec\'s ', h('code', null, detail.spec.server_prefix), ' prefix') : null,
        readOnly ? null : h('div', { class: 'row' }, h('span', { class: 'grow' }), saveBtn));
      form.addEventListener('input', () => { dirty = true; });
      form.addEventListener('change', () => { dirty = true; });
      saveBtn.addEventListener('click', async () => {
        const responses = { mode: mode.value, seed: Number(seed.value) || 0, delay_ms: Number(delay.value) || 0, validate_requests: validate.value, disabled_status: Number(disabled.value) };
        saveBtn.disabled = true;
        const rr = await api('PATCH', '/' + enc(name), { responses, strip_server_prefix: strip.checked });
        saveBtn.disabled = false;
        if (report(rr, 'Saved to the draft. Deploy to publish it.', 'Saving')) { dirty = false; refresh(); }
      });
      // Rename.
      const nameIn = input({ value: name, class: 'input mono', id: 'mock-rename', 'aria-label': 'New name', maxlength: '40' });
      const nameMsg = h('span', { class: 'hint' }, 'The URL changes to /mock/<new name>; clients using the old URL get 404.');
      const renameBtn = h('button', { class: 'btn', type: 'button' }, 'Rename');
      renameBtn.addEventListener('click', async () => {
        const n = nameIn.value.trim();
        const why = n === name ? 'That is the current name.' : nameProblem(n);
        if (why) { nameMsg.textContent = why; nameMsg.classList.add('err-text'); return; }
        const rr = await api('PATCH', '/' + enc(name), { name: n });
        if (report(rr, `Renamed to ${n}`, 'Renaming')) location.hash = '#/mocks/' + enc(n) + '/settings';
        else { nameMsg.textContent = errorText(rr); nameMsg.classList.add('err-text'); }
      });
      // Replace the spec.
      const specIn = textarea({ rows: 6, placeholder: 'Paste the new version of the spec (JSON or YAML), or drop a file here', 'aria-label': 'New spec' });
      const specMsg = h('div');
      const fileIn = h('input', { type: 'file', accept: '.json,.yaml,.yml', hidden: true });
      const loadFile = async (f) => { try { specIn.value = await readFile(f, detail.limits.max_spec_bytes); } catch (e) { replace(specMsg, notice('err', e.message)); } };
      fileIn.addEventListener('change', () => { if (fileIn.files[0]) loadFile(fileIn.files[0]); fileIn.value = ''; });
      wireDrop(specIn, loadFile);
      const replaceBtn = h('button', { class: 'btn', type: 'button' }, icon('upload'), 'Replace the spec in the draft');
      replaceBtn.addEventListener('click', async () => {
        if (!specIn.value.trim()) { replace(specMsg, notice('warn', 'Paste or drop the new spec first.')); return; }
        replaceBtn.disabled = true;
        replace(specMsg, spinner('Checking the spec'));
        const rr = await api('PATCH', '/' + enc(name), { spec: specIn.value }, { timeout: 60000 });
        replaceBtn.disabled = false;
        if (rr.ok && rr.json) {
          specIn.value = '';
          const rep = rr.json.report;
          replace(specMsg, notice(rep.ok ? 'ok' : 'warn', `Spec replaced in the draft (${rr.json.operations.length} operations, ${rep.errors} check errors, ${rep.warnings} warnings). Operation settings were kept by operationId. Deploy to publish it.`));
          refresh();
        } else replace(specMsg, notice('err', errorText(rr, 'Replacing the spec')));
      });
      const d = detail;
      replace(el,
        card('Behaviour', { hint: 'saved to the draft' }, form),
        h('div', { class: 'grid grid-2' },
          readOnly || detail.yours ? null : card('Name', null, h('div', { class: 'stack-sm' }, h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, nameIn), renameBtn), nameMsg)),
          card('About', null, kvgrid([
            ['Origin', d.origin === 'sample' ? 'built-in sample' : d.origin === 'file' ? 'config file (changes are overwritten on reload)' : 'created here or with the API'],
            ['Spec', (String(d.spec.openapi || '').startsWith('2') ? 'Swagger ' : 'OpenAPI ') + d.spec.openapi], ['Server prefix', d.spec.server_prefix || 'none'],
            ['Live revision', d.live_revision === null ? 'none' : String(d.live_revision)], ['Draft revision', String(d.draft_revision)],
            ['Created', new Date(d.created_at).toLocaleString()], ['Updated', relTime(d.updated_at)],
            ['Expires', d.expires_at ? new Date(d.expires_at).toLocaleString() : 'never']]))),
        readOnly ? null : card('Replace the spec', { hint: 'a new version of the same API' }, h('div', { class: 'stack-sm' }, specIn, fileIn,
          h('div', { class: 'row' }, h('button', { class: 'btn sm', type: 'button', onclick: () => fileIn.click() }, icon('file'), 'Browse files'), h('span', { class: 'grow' }), replaceBtn), specMsg)),
        readOnly ? null : card('Danger zone', null, h('div', { class: 'row' }, h('p', { class: 'small grow', style: { margin: 0 } }, 'Deleting removes the mock, its revisions and its traffic. Its URL answers 404 afterwards.'),
          h('button', { class: 'btn danger', type: 'button', onclick: deleteMock }, icon('trash'), 'Delete mock'))));
    }
    return { el, update() { if (!dirty) render(); } };
  }

  // ── Data (stateful mode) ──
  function dataPanel() {
    const el = h('div', { class: 'stack' });
    const out = h('div');
    const usage = h('div', { class: 'stack-sm' });
    let timer = null;
    const resetBtn = readOnly ? null : h('button', { class: 'btn sm danger', type: 'button', onclick: reset }, icon('trash'), 'Reset');
    el.append(
      notice('', 'Stateful mode remembers changes in memory: POST stores an item, GET reads it back, PUT and PATCH update it, DELETE removes it. Collections start from the list operation\'s example. Rules, custom responses and the Prefer header still win; responses say ', h('code', null, 'X-Rustybin-Mock-Source: state create'), ' (read, update, delete, list, miss).'),
      card('Stored data', { actions: h('div', { class: 'row' }, h('button', { class: 'btn sm', type: 'button', onclick: load }, icon('refresh'), 'Refresh'), resetBtn) }, usage, out));

    async function load() {
      const r = await api('GET', `/${enc(name)}/state`);
      if (!alive) return;
      if (!r.ok || !r.json) { replace(out, notice('err', errorText(r, 'Loading the data'))); return; }
      const st = r.json;
      const pct = st.max_items ? Math.min(100, Math.round((st.items / st.max_items) * 100)) : 0;
      replace(usage, h('div', { class: 'row' }, h('strong', null, plural(st.items, 'item')), h('span', { class: 'small muted' }, `of at most ${st.max_items}, ${Math.round(st.bytes / 1024 * 10) / 10} KB of ${Math.round(st.max_bytes / 1024)} KB`)),
        h('div', { class: 'progress', role: 'progressbar', 'aria-valuenow': String(pct), 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-label': 'Store usage' }, h('div', { style: { width: pct + '%' } })));
      const cols = Object.entries(st.collections || {});
      if (!cols.length) {
        replace(out, empty('Nothing stored yet', 'Call the mock (Try it, curl or your gateway): collections appear here as soon as they are read or changed.', 'inbox'));
        return;
      }
      replace(out, h('div', { class: 'stack-sm' }, ...cols.map(([path, items]) => h('details', { class: 'report-op data-col', open: cols.length < 4 },
        h('summary', null, h('span', { class: 'mono grow' }, path), badge(plural(items.length, 'item'), 'info')),
        jsonView(items, 'short')))));
    }

    async function reset() {
      if (!confirm(`Reset the stored data of ${name}? Every collection goes back to the spec's examples.`)) return;
      const r = await api('DELETE', `/${enc(name)}/state`);
      if (report(r, 'Stored data reset', 'Resetting')) { load(); refresh(true); }
    }

    return {
      el,
      show() {
        load();
        clearInterval(timer);
        timer = setInterval(() => { if (!alive || !el.isConnected) { clearInterval(timer); return; } if (!document.hidden) load(); }, 4000);
      },
      update() { if (el.isConnected) load(); },
    };
  }

  // ── Deploy report ──
  function reportPanel() {
    const el = h('div', { class: 'stack' });
    const out = h('div');
    const revEl = h('div');
    let loaded = false;
    const runBtn = h('button', { class: 'btn sm', type: 'button', onclick: load }, icon('refresh'), 'Run checks');
    el.append(card('Deploy checks of the draft', { actions: runBtn }, out), h('div', { id: 'mock-revisions' }, revEl));

    async function load() {
      loaded = true;
      replace(out, spinner('Running the checks'));
      const r = await api('GET', `/${enc(name)}/report`, undefined, { timeout: 60000 });
      if (!alive) return;
      if (!r.ok || !r.json) { replace(out, notice('err', errorText(r, 'The report'))); return; }
      const rep = r.json;
      const ops = rep.operations || [];
      replace(out, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, rep.ok ? badge(h('span', null, icon('check'), ' ready to deploy'), 'ok') : badge(h('span', null, icon('alert'), ' errors block a deploy'), 'err'),
          badge(rep.errors + ' errors', rep.errors ? 'err' : ''), badge(rep.warnings + ' warnings', rep.warnings ? 'warn' : ''),
          h('span', { class: 'small muted' }, 'Each operation gets a valid sample request; every response it can send is checked against its schema.')),
        rep.spec_warnings.length ? h('details', { class: 'disclosure' }, h('summary', null, `${rep.spec_warnings.length} spec warnings`), h('ul', { class: 'issue-list small' }, ...rep.spec_warnings.map((w) => h('li', null, w)))) : null,
        rep.findings.length ? h('ul', { class: 'issue-list small' }, ...rep.findings.map((f) => h('li', null, badge(f.level, f.level === 'error' ? 'err' : 'warn'), ' ', f.message))) : null,
        h('div', { class: 'report-ops' }, ...ops.map((o) => {
          const req = sampleToRequest(name, o.request);
          return h('details', { class: 'report-op', open: o.status === 'error' },
            h('summary', null, checkIcon(o.status), methodBadge(o.method), h('span', { class: 'mono ellipsis grow' }, o.path), h('span', { class: 'tiny muted' }, o.id),
              o.findings.length ? badge(String(o.findings.length), o.status === 'error' ? 'err' : 'warn') : null),
            h('div', { class: 'stack-sm', style: { padding: '8px 0 12px' } },
              o.findings.length ? h('ul', { class: 'issue-list small' }, ...o.findings.map((f) => h('li', { class: f.level === 'error' ? 'err-text' : '' }, f.message))) : null,
              responseChecks(o.responses),
              copyableCode(toCurl({ method: req.method, url: req.url, headers: req.headers, body: req.body })),
              readOnly ? null : h('div', null, h('button', { class: 'btn sm', type: 'button', onclick: () => goTab('operations', o.id) }, icon('edit'), 'Edit this operation'))));
        }))));
    }

    function renderRevisions() {
      const d = detail;
      replace(revEl, card('Revisions', { hint: `the last ${d.limits.revisions} deployed revisions are kept` },
        h('div', { class: 'list rev-list' }, ...d.revisions.map((rv) => h('div', { class: 'list-item rev-row' },
          h('span', { class: 'mono' }, 'r' + rv.revision),
          h('span', { class: 'small' }, new Date(rv.created_at).toLocaleString(), h('span', { class: 'muted' }, ' (' + relTime(rv.created_at) + ')')),
          h('span', { class: 'row nowrap', style: { gap: '6px' } },
            rv.live ? badge('live', 'ok') : null, rv.draft ? badge('draft', 'warn') : null,
            !rv.live && !rv.draft && !readOnly ? h('button', { class: 'btn sm', type: 'button', onclick: () => rollback(rv.revision) }, icon('history'), 'Roll back to this') : null)))),
        d.revisions.length < 2 ? h('p', { class: 'small muted', style: { margin: '10px 0 0' } }, 'Earlier revisions appear here after the next deploy; roll back to any of them in one click.') : null));
    }

    async function rollback(rev) {
      if (!confirm(`Make revision ${rev} live again? The current draft is kept.`)) return;
      const r = await api('POST', `/${enc(name)}/rollback`, { revision: rev });
      if (report(r, `Revision ${rev} is live`, 'Rolling back')) refresh();
    }

    return {
      el,
      update() { renderRevisions(); if (loaded) load(); },
      show() { if (!loaded) load(); },
    };
  }

  // Keep the header (calls, last hit) fresh while the page is open.
  pollTimer = setInterval(() => { if (!document.hidden && detail) refresh(true); }, 10000);
  route(sub);
  refresh().then(() => {
    if (!alive || !tabCtl) return;
    route(sub);
  });

  return {
    route,
    destroy() {
      alive = false;
      clearInterval(pollTimer);
      document.removeEventListener('click', closeMenu);
      if (editor) editor.destroy();
      if (panels.traffic) panels.traffic.destroy();
    },
  };
}

