// Operation editor of a mock: pick the default response (status, spec
// example, generated data or a custom body built with a schema-driven form),
// headers, delay, public access and the rules evaluated before it. Saving
// writes the operation's settings into the draft; the detail header then
// offers Deploy.
import { h, replace, clear, card, icon, notice, spinner, methodBadge, badge, input, select, textarea, segmented, toast, copyButton, debounce } from '../../lib/dom.js';
import { jsonView, copyableCode } from '../../lib/format.js';
import { toCurl } from '../../lib/http.js';
import { headerEditor } from '../../lib/widgets.js';
import { schemaEditor } from '../../lib/schemaeditor.js';
import { api, errorText, sampleToRequest, relTime } from '../../lib/mockapi.js';

import { storedExamplesList } from './ai.js';

const SOURCES = [['example', 'Example'], ['generated', 'Generated'], ['stateful', 'Stateful'], ['custom', 'Custom']];
const OPERATORS = [['equals', 'equals'], ['not_equals', 'does not equal'], ['exists', 'is present'], ['missing', 'is missing'], ['matches', 'matches regex']];
const WHERE = [['path', 'path parameter'], ['query', 'query parameter'], ['header', 'header'], ['body', 'body field']];

function clone(v) {
  return v === undefined ? undefined : JSON.parse(JSON.stringify(v));
}

/** Media of a documented response the mock answers with (JSON first). */
function mediaOf(resp) {
  const content = (resp && resp.content) || [];
  return content.find((m) => /json/i.test(m.media_type)) || content[0] || null;
}

function isJsonMedia(media) {
  return !media || /json|\*\/\*/i.test(media.media_type);
}

/** Checks status of an operation as an icon. */
export function checkIcon(status) {
  if (status === 'error') return h('span', { class: 'check-icon err', title: 'Deploy checks: error' }, icon('alert'));
  if (status === 'warning') return h('span', { class: 'check-icon warn', title: 'Deploy checks: warning' }, icon('alert'));
  if (status === 'ok') return h('span', { class: 'check-icon ok', title: 'Deploy checks: ok' }, icon('check'));
  return null;
}

/**
 * opts: {name, opId, readOnly, mockMode (the mock's response mode),
 * securityOn, onSaved(), onTry(opId)}. Returns {el, destroy, dirty()}.
 */
export function operationEditor(opts) {
  const root = h('div', { class: 'stack' }, card(null, null, spinner('Loading the operation')));
  let alive = true;
  let data = null;
  let initial = '';
  let dirtyFlag = false;
  const dirtyEl = h('span', { class: 'badge warn', hidden: true }, 'Unsaved changes');
  const saveBtn = h('button', { class: 'btn primary', type: 'button', disabled: true }, 'Save to draft');
  const saveMsg = h('div');

  // Response state.
  let status = 0;
  let source = 'example';
  let exampleName = '';
  let customBody;
  let customText = '';
  let editMode = 'form';
  let previewSeed = 0;
  let form = null;
  const respEl = h('div', { class: 'stack-sm' });
  const hdrs = headerEditor([]);
  const delayIn = input({ type: 'number', min: '0', id: 'op-delay', class: 'input mono', placeholder: 'mock default', 'aria-label': 'Delay (ms)', style: { maxWidth: '9em' } });
  // The slider and the number field edit the same value (empty: mock default);
  // the slider covers the usual range, the field takes anything up to the limit.
  const delayMax = Math.max(1000, Math.min(Number(opts.maxDelayMs) || 5000, 5000));
  const delayRange = h('input', { type: 'range', min: '0', max: String(delayMax), step: '50', class: 'grow', 'aria-label': 'Delay slider (ms)' });
  delayRange.addEventListener('input', () => { delayIn.value = delayRange.value; });
  delayIn.addEventListener('input', () => { delayRange.value = delayIn.value.trim() === '' ? '0' : delayIn.value; });
  const publicCb = h('input', { type: 'checkbox' });
  let rules = null;

  async function load() {
    const r = await api('GET', `/${encodeURIComponent(opts.name)}/operations/${encodeURIComponent(opts.opId)}`);
    if (!alive) return;
    if (!r.ok || !r.json) {
      replace(root, card(null, null, notice('err', errorText(r, 'Loading the operation'))));
      return;
    }
    init(r.json);
  }

  function init(d) {
    data = d;
    const s = d.settings || {};
    const resp = s.respond || {};
    status = resp.status || d.default_status;
    customBody = clone(resp.body);
    exampleName = resp.example || '';
    if (resp.body !== undefined) source = 'custom';
    else if (resp.example) source = 'example';
    else {
      const mode = s.mode || opts.mockMode;
      source = mode === 'generated' ? 'generated' : mode === 'stateful' ? 'stateful' : mode === 'static' ? 'custom' : 'example';
    }
    customText = customBody === undefined ? '' : JSON.stringify(customBody, null, 2);
    hdrs.set(Object.entries(resp.headers || {}));
    delayIn.value = s.delay_ms !== undefined ? String(s.delay_ms) : '';
    delayRange.value = s.delay_ms !== undefined ? String(s.delay_ms) : '0';
    publicCb.checked = !!s.public;
    if (rules) rules.destroy();
    rules = rulesBuilder(clone(s.rules) || [], d, opts, markDirty);
    render();
    initial = JSON.stringify(safeBuild());
    markDirty();
  }

  function statuses() {
    const list = [];
    for (const r of data.operation.responses || []) {
      const n = Number(r.status);
      if (Number.isInteger(n) && n >= 100 && n <= 599) list.push(n);
    }
    if (!list.includes(status)) list.push(status);
    return list;
  }

  function responseFor(code) {
    const all = data.operation.responses || [];
    return all.find((r) => Number(r.status) === code) || all.find((r) => r.status === String(code)[0] + 'XX') || all.find((r) => r.status === 'default') || null;
  }

  function render() {
    const op = data.operation;
    const req = sampleToRequest(opts.name, data.sample_request);
    const curl = toCurl({ method: req.method, url: req.url, headers: req.headers, body: req.body });
    const ro = opts.readOnly;
    replace(root,
      card(null, null, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, methodBadge(op.method), h('h2', { class: 'mono break grow', style: { fontSize: '15px' } }, op.path),
          op.deprecated ? badge('deprecated', 'warn') : null,
          (op.security || []).length ? badge(h('span', null, icon('lock'), opts.securityOn && !publicCb.checked ? ' secured' : ' declares security'), opts.securityOn && !publicCb.checked ? 'violet' : 'outline') : null),
        op.summary ? h('p', { style: { fontWeight: 550, margin: 0 } }, op.summary) : null,
        op.description ? h('p', { class: 'small muted clamp-3', style: { margin: 0 } }, op.description) : null,
        h('div', { class: 'row nowrap' }, h('code', { class: 'grow break small' }, req.url), copyButton(curl, { label: 'Copy curl', toast: 'curl command copied' }),
          h('button', { class: 'btn sm', type: 'button', onclick: () => opts.onTry && opts.onTry(opts.opId) }, icon('send'), 'Try it')))),
      card('Default response', { hint: 'what this operation answers when no rule matches' }, respEl),
      card('Rules', { hint: 'checked in order before the default response; the first match wins' }, h('fieldset', { class: 'plain', disabled: ro }, rules.el), rules.preview),
      card('Options', null, h('fieldset', { class: 'plain stack-sm', disabled: ro },
        h('div', { class: 'field' }, h('span', null, 'Response headers'), hdrs.el,
          headerHint()),
        h('div', { class: 'grid grid-2' },
          h('div', { class: 'field' }, h('label', { for: 'op-delay' }, 'Delay (ms)'), h('div', { class: 'row nowrap' }, delayRange, delayIn), h('span', { class: 'hint' }, `Extra latency for this operation only (slider up to ${delayMax} ms, type more in the field; empty uses the mock default).`)),
          h('div', { class: 'field' }, h('span', null, 'Access'),
            h('label', { class: 'check' }, publicCb, 'Public (no mocked security)'),
            h('span', { class: 'hint' }, opts.securityOn ? 'Skips the credential check for this operation.' : 'Mocked security is off for this mock, so every operation is public.'))))),
      ro ? null : h('div', { class: 'save-bar row' }, dirtyEl, h('span', { class: 'grow' }), saveMsg,
        h('button', { class: 'btn', type: 'button', onclick: resetAll, title: 'Remove every custom setting of this operation' }, 'Reset to spec'),
        h('button', { class: 'btn', type: 'button', onclick: () => { if (!dirtyFlag || confirm('Discard the unsaved changes of this operation?')) init(data); } }, 'Discard'),
        saveBtn),
      reportCard(curl));
    renderResponse();
  }

  function headerHint() {
    const resp = responseFor(status);
    const documented = ((resp && resp.headers) || []).map((x) => x.name);
    return h('span', { class: 'hint' }, documented.length
      ? 'Documented for ' + status + ': ' + documented.join(', ') + ' (generated unless set here).'
      : 'Added to the response; Content-Type comes from the spec.');
  }

  // ── Default response ──

  function renderResponse() {
    const resp = responseFor(status);
    const media = mediaOf(resp);
    const examples = (media && media.examples) || [];
    const hasUnnamed = media && media.example !== undefined;
    const tabsEl = h('div', { class: 'status-tabs', role: 'tablist', 'aria-label': 'Response status' });
    for (const code of statuses()) {
      const b = h('button', { type: 'button', role: 'tab', 'aria-selected': String(code === status), class: 'st-tab' },
        h('span', { class: 'st st-' + Math.floor(code / 100) }, String(code)), code === data.default_status ? h('span', { class: 'tiny muted' }, 'default') : null);
      b.addEventListener('click', () => { if (code !== status) changeStatus(code); });
      tabsEl.appendChild(b);
    }
    const other = h('input', { class: 'input sm mono', type: 'number', min: '100', max: '599', placeholder: 'other', 'aria-label': 'Other status', style: { width: '84px' } });
    other.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); addOther(); } });
    function addOther() {
      const n = Number(other.value);
      if (Number.isInteger(n) && n >= 100 && n <= 599) changeStatus(n);
      else toast('A status between 100 and 599', 'err');
    }
    tabsEl.appendChild(h('span', { class: 'row nowrap', style: { gap: '4px' } }, other, h('button', { class: 'btn sm', type: 'button', onclick: addOther }, icon('plus'), 'Add status')));
    const seg = segmented(SOURCES.filter(([v]) => !opts.visitor || v !== 'stateful'), source, (v) => {
      if (v === 'custom' && customBody === undefined && !customText) fillCustom('preview');
      source = v;
      renderResponse();
      markDirty();
    }, 'Response body source');
    const detail = h('div', { class: 'stack-sm' });
    replace(respEl,
      h('div', { class: 'field' }, h('span', null, 'Responds with'), tabsEl,
        resp ? h('span', { class: 'hint' }, resp.description || '') : h('span', { class: 'hint' }, 'Not documented in the spec: the body comes only from a custom body.')),
      h('div', { class: 'field' }, h('span', null, 'Body'), seg.el),
      detail);
    if (source === 'example') {
      const opts2 = [];
      if (hasUnnamed) opts2.push(['', 'Spec example']);
      for (const ex of examples) opts2.push([ex.name, ex.name + (ex.summary ? ': ' + ex.summary : '')]);
      const stored = ((data.settings && data.settings.examples) || {})[String(status)] || [];
      for (const ex of stored) opts2.push([ex.name, ex.name + ' (' + ex.source + ')']);
      if (!opts2.length) {
        detail.appendChild(notice('', 'The spec has no example for ' + status + ': the mock generates one from the schema. ',
          opts.readOnly ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => opts.onGenerate && opts.onGenerate(opts.opId) }, icon('ai'), 'Generate examples')));
        exampleName = '';
      } else {
        if (!opts2.some(([v]) => v === exampleName)) exampleName = opts2[0][0];
        const sel = select(opts2, exampleName, { 'aria-label': 'Example' });
        sel.addEventListener('change', () => { exampleName = sel.value; markDirty(); preview(previewEl, 'examples'); });
        if (opts2.length > 1 || examples.length) detail.appendChild(h('label', { class: 'field' }, h('span', null, 'Example'), sel));
      }
      const previewEl = h('div');
      detail.appendChild(previewEl);
      preview(previewEl, 'examples');
      const storedList = storedExamplesList(data.settings && data.settings.examples, status);
      if (storedList) detail.appendChild(storedList);
    } else if (source === 'generated') {
      const previewEl = h('div');
      detail.append(h('p', { class: 'small muted', style: { margin: 0 } }, 'Data generated from the response schema with the mock\'s seed: the same request gets the same data.'), previewEl);
      preview(previewEl, 'generated');
    } else if (source === 'stateful') {
      detail.appendChild(notice('', h('strong', null, 'Answers from the stored data. '),
        'POST on a collection stores the item, GET reads it back (or lists the collection, with offset and limit), PUT and PATCH update it, DELETE removes it. ',
        'Unknown ids answer 404; when the operation does not look like CRUD, the spec example answers. Rules and the Prefer header still win.',
        opts.onData ? h('div', { style: { marginTop: '6px' } }, h('button', { class: 'btn sm', type: 'button', onclick: opts.onData }, icon('data'), 'See the stored data')) : null));
    } else {
      detail.appendChild(customEditor(resp, media));
    }
  }

  function changeStatus(code) {
    status = code;
    customBody = undefined;
    customText = '';
    exampleName = '';
    if (source === 'custom') source = mediaOf(responseFor(code)) ? 'example' : 'custom';
    renderResponse();
    markDirty();
  }

  async function sample(mode, seed) {
    const q = new URLSearchParams({ status: String(status), mode });
    if (mode === 'examples' && exampleName) q.set('example', exampleName);
    if (seed) q.set('seed', String(seed));
    return api('GET', `/${encodeURIComponent(opts.name)}/operations/${encodeURIComponent(opts.opId)}/sample?${q}`);
  }

  async function preview(el, mode) {
    replace(el, spinner('Rendering the response'));
    const r = await sample(mode, mode === 'generated' ? previewSeed : 0);
    if (!alive || !el.isConnected) return;
    if (!r.ok || !r.json) { replace(el, notice('err', errorText(r, 'The sample'))); return; }
    const resp = r.json.response;
    const body = resp.body;
    replace(el, h('div', { class: 'stack-sm' },
      h('div', { class: 'row' }, h('span', { class: 'label' }, 'Preview'), badge(resp.content_type || 'no body', 'outline'), badge(resp.source, 'info'),
        resp.valid ? badge(h('span', null, icon('check'), ' matches the schema'), 'ok') : badge(h('span', null, icon('alert'), ' does not match the schema'), 'err'),
        h('span', { class: 'grow' }),
        mode === 'generated' ? h('button', { class: 'btn sm', type: 'button', onclick: () => { previewSeed = Math.floor(Math.random() * 1e9) + 2; preview(el, mode); } }, icon('refresh'), 'Regenerate') : null,
        opts.readOnly || body === null || body === undefined ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => { customBody = clone(body); customText = JSON.stringify(body, null, 2); source = 'custom'; renderResponse(); markDirty(); } }, icon('edit'), 'Edit as custom body')),
      body === null || body === undefined ? h('p', { class: 'small muted' }, 'Empty body.') : jsonView(body, 'short'),
      mode === 'generated' && previewSeed ? h('p', { class: 'tiny muted' }, 'Previewing seed ' + previewSeed + '. Calls use the mock\'s seed (Settings tab); use "Edit as custom body" to keep this data.') : null,
      issuesList(resp.issues)));
  }

  // ── Custom body: schema form or JSON ──

  function customEditor(resp, media) {
    const schema = media && media.schema;
    const json = isJsonMedia(media);
    const wrap = h('div', { class: 'stack-sm' });
    const issuesEl = h('div');
    const jsonIn = textarea({ rows: 14, 'aria-label': 'Custom body (JSON)', placeholder: json ? '{ }' : 'Response body text' });
    jsonIn.value = customText;
    jsonIn.readOnly = !!opts.readOnly;
    const formHost = h('div');
    const useForm = !!schema && json;
    if (!useForm) editMode = 'json';
    const validateSoon = debounce(validate, 400);
    if (useForm) {
      form = schemaEditor(schema, customBody, {
        readOnly: opts.readOnly,
        onChange: (v) => { customBody = v; customText = JSON.stringify(v, null, 2); markDirty(); validateSoon(); },
      });
      formHost.appendChild(form.el);
    } else {
      form = null;
    }
    jsonIn.addEventListener('input', () => {
      customText = jsonIn.value;
      try { customBody = jsonIn.value.trim() ? JSON.parse(jsonIn.value) : undefined; jsonIn.classList.remove('invalid'); } catch { customBody = json ? undefined : jsonIn.value; jsonIn.classList.toggle('invalid', json); }
      markDirty();
      validateSoon();
    });
    const modeSeg = useForm ? segmented([['form', 'Form'], ['json', 'JSON']], editMode, (v) => {
      if (v === 'form') {
        if (jsonIn.value.trim()) {
          try { customBody = JSON.parse(jsonIn.value); } catch (e) { modeSeg.set('json'); toast('Fix the JSON first: ' + e.message, 'err'); return; }
        }
        form.set(customBody);
      } else {
        jsonIn.value = customBody === undefined ? '' : JSON.stringify(form.get(), null, 2);
        customText = jsonIn.value;
      }
      editMode = v;
      formHost.hidden = v !== 'form';
      jsonIn.hidden = v !== 'json';
      validate();
    }, 'Editor') : null;
    formHost.hidden = editMode !== 'form';
    jsonIn.hidden = editMode !== 'json';
    const fillEx = opts.readOnly ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => fillCustom('examples') }, 'Fill from example');
    const fillGen = opts.readOnly ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => fillCustom('generated') }, 'Fill with generated data');
    wrap.append(
      h('div', { class: 'row' }, modeSeg ? modeSeg.el : h('span', { class: 'small muted' }, schema ? 'JSON' : 'No schema documented: free JSON or text.'),
        schema && schema.title ? badge(schema.title, 'outline') : null, h('span', { class: 'grow' }), fillEx, fillGen),
      formHost, jsonIn, issuesEl,
      h('p', { class: 'tiny muted', style: { margin: 0 } }, 'Leave the body empty for an empty response. ', json ? 'Checked live against the spec\'s schema.' : ''));

    async function validate() {
      if (!alive || !wrap.isConnected) return;
      if (editMode === 'json' && json && jsonIn.value.trim()) {
        try { JSON.parse(jsonIn.value); } catch (e) { replace(issuesEl, notice('err', 'Not valid JSON: ' + e.message)); return; }
      }
      if (!schema || customBody === undefined) { replace(issuesEl); if (form) form.showIssues([]); return; }
      const body = editMode === 'form' && form ? form.get() : customBody;
      const r = await api('POST', `/${encodeURIComponent(opts.name)}/operations/${encodeURIComponent(opts.opId)}/validate`, { kind: 'response', status, body });
      if (!alive || !wrap.isConnected || !r.ok || !r.json) return;
      const issues = r.json.issues || [];
      const rest = form && editMode === 'form' ? form.showIssues(issues) : issues;
      if (!issues.length) replace(issuesEl, h('span', { class: 'badge ok' }, icon('check'), ' matches the schema'));
      else replace(issuesEl, notice('warn', h('strong', null, `${issues.length} problem${issues.length === 1 ? '' : 's'} with the schema`),
        rest.length ? issuesList(rest) : h('div', { class: 'small' }, 'Marked on the fields above.')));
    }
    if (customBody !== undefined) setTimeout(validate, 0);
    return wrap;
  }

  async function fillCustom(kind) {
    const mode = kind === 'generated' ? 'generated' : 'examples';
    const r = await sample(mode, kind === 'generated' ? Math.floor(Math.random() * 1e9) + 2 : 0);
    if (!alive) return;
    if (!r.ok || !r.json) { toast(errorText(r, 'The sample'), 'err'); return; }
    const body = r.json.response.body;
    customBody = body === null ? undefined : clone(body);
    customText = customBody === undefined ? '' : JSON.stringify(customBody, null, 2);
    source = 'custom';
    renderResponse();
    markDirty();
  }

  // ── Save ──

  function safeBuild() {
    try { return build(); } catch (e) { return { invalid: e.message }; }
  }

  /** Operation settings from the editor state (throws on invalid input). */
  function build() {
    const out = {};
    // Stored examples (AI, generated, user) are kept as they are.
    if (data.settings && data.settings.examples && Object.keys(data.settings.examples).length) out.examples = clone(data.settings.examples);
    const respond = {};
    if (status !== data.default_status) respond.status = status;
    const hs = hdrs.get();
    if (hs.length) respond.headers = Object.fromEntries(hs);
    if (source === 'example') {
      if (exampleName) respond.example = exampleName;
      if (opts.mockMode !== 'examples') out.mode = 'examples';
    } else if (source === 'generated') {
      if (opts.mockMode !== 'generated') out.mode = 'generated';
    } else if (source === 'stateful') {
      if (opts.mockMode !== 'stateful') out.mode = 'stateful';
    } else {
      const media = mediaOf(responseFor(status));
      if (editMode === 'json' && customText.trim()) {
        try { respond.body = JSON.parse(customText); } catch (e) {
          if (isJsonMedia(media)) throw new Error('The custom body is not valid JSON: ' + e.message);
          respond.body = customText;
        }
      } else if (editMode === 'form' && form && customBody !== undefined) {
        respond.body = form.get();
      } else if (customBody !== undefined) {
        respond.body = customBody;
      }
      if (respond.body === undefined && opts.mockMode !== 'static') out.mode = 'static';
    }
    if (Object.keys(respond).length) out.respond = respond;
    const rs = rules.get();
    if (rs.length) out.rules = rs;
    if (delayIn.value.trim() !== '') {
      const d = Number(delayIn.value);
      if (!Number.isInteger(d) || d < 0) throw new Error('The delay must be a whole number of milliseconds.');
      out.delay_ms = d;
    }
    if (publicCb.checked) out.public = true;
    return out;
  }

  function markDirty() {
    if (!data) return;
    const now = JSON.stringify(safeBuild());
    dirtyFlag = now !== initial;
    dirtyEl.hidden = !dirtyFlag;
    saveBtn.disabled = !dirtyFlag;
  }

  async function save(settings) {
    replace(saveMsg);
    saveBtn.disabled = true;
    const r = await api('PUT', `/${encodeURIComponent(opts.name)}/operations/${encodeURIComponent(opts.opId)}`, settings);
    if (!alive) return;
    if (!r.ok || !r.json) {
      saveBtn.disabled = false;
      replace(saveMsg, h('span', { class: 'err-text small' }, errorText(r, 'Saving')));
      return;
    }
    toast('Saved to the draft. Deploy to publish it.');
    init(r.json);
    if (opts.onSaved) opts.onSaved();
  }

  // Any edit (typing, toggles, added or removed rows) refreshes the dirty state.
  root.addEventListener('input', markDirty);
  root.addEventListener('change', markDirty);
  root.addEventListener('click', (e) => { if (e.target.closest('button')) setTimeout(markDirty, 0); });

  saveBtn.addEventListener('click', () => {
    let s;
    try { s = build(); } catch (e) { replace(saveMsg, h('span', { class: 'err-text small' }, e.message)); return; }
    save(Object.keys(s).length ? s : null);
  });

  function resetAll() {
    if (!confirm('Remove every custom setting of this operation (response, rules, headers, delay)? It answers from the spec again after the next deploy.')) return;
    save(null);
  }

  // ── Checks ──

  function reportCard(curl) {
    const rep = data.report;
    if (!rep) return card('Deploy checks', null, h('p', { class: 'small muted' }, 'No check result yet.'));
    return card('Deploy checks', { actions: checkIcon(rep.status) },
      h('div', { class: 'stack-sm' },
        rep.findings.length ? h('ul', { class: 'issue-list small' }, ...rep.findings.map((f) => h('li', { class: f.level === 'error' ? 'err-text' : '' }, badge(f.level, f.level === 'error' ? 'err' : 'warn'), ' ', f.message))) : h('p', { class: 'small muted', style: { margin: 0 } }, 'No findings: a valid request gets a schema-valid response.'),
        responseChecks(rep.responses),
        h('details', { class: 'disclosure' }, h('summary', null, 'Sample request as curl'), copyableCode(curl))));
  }

  load();
  return {
    el: root,
    destroy() { alive = false; if (rules) rules.destroy(); },
    /** Re-read the captured calls (new traffic) for the rule preview. */
    refreshPreview() { if (rules) rules.reload(); },
    dirty: () => dirtyFlag,
  };
}

/** Table of the response checks of an operation report. */
export function responseChecks(list) {
  if (!list || !list.length) return null;
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
    h('thead', null, h('tr', null, h('th', null, 'Case'), h('th', null, 'Status'), h('th', null, 'Body'), h('th', null, 'Schema'))),
    h('tbody', null, ...list.map((c) => h('tr', null,
      h('td', { class: 'mono' }, c.source), h('td', null, h('span', { class: 'st st-' + Math.floor(c.status / 100) }, String(c.status))),
      h('td', { class: 'mono' }, c.body_source),
      h('td', null, c.valid ? badge('valid', 'ok') : h('div', null, badge('invalid', 'err'), issuesList(c.issues))))))));
}

/** [{pointer, message}] as a list. */
export function issuesList(issues) {
  if (!issues || !issues.length) return null;
  return h('ul', { class: 'issue-list small' }, ...issues.map((i) => h('li', null, h('code', null, i.pointer || '/'), ' ', i.message)));
}

// ── Rules ──────────────────────────────────────────────────────────

function parseWhen(key) {
  if (key === 'body') return ['body', ''];
  const i = key.indexOf('.');
  if (i < 0) return ['body', key];
  return [key.slice(0, i), key.slice(i + 1)];
}

function condOp(c) {
  if (c.equals !== undefined) return ['equals', String(c.equals)];
  if (c.not_equals !== undefined) return ['not_equals', String(c.not_equals)];
  if (c.matches !== undefined) return ['matches', c.matches];
  if (c.exists === false) return ['missing', ''];
  return ['exists', ''];
}

/** Editable rule list. Returns {el, get() -> rules}. */
function rulesBuilder(initialRules, data, opts, notify) {
  const op = data.operation;
  let alive = true;
  // Every change also refreshes the "would match" preview (debounced).
  const onChange = () => { notify(); previewSoon(); };
  const params = { path: [], query: [], header: [] };
  for (const p of op.parameters || []) if (params[p.in]) params[p.in].push(p.name);
  const statuses = (op.responses || []).map((r) => Number(r.status)).filter((n) => Number.isInteger(n));
  const examplesFor = (code) => {
    const resp = (op.responses || []).find((r) => Number(r.status) === Number(code));
    const media = mediaOf(resp);
    return ((media && media.examples) || []).map((e) => e.name);
  };
  let list = initialRules.map((r) => ({
    name: r.name || '',
    conds: Object.entries(r.when || {}).map(([k, c]) => { const [where, name] = parseWhen(k); const [opn, value] = condOp(c || {}); return { where, name, op: opn, value }; }),
    status: r.respond && r.respond.status ? r.respond.status : '',
    example: (r.respond && r.respond.example) || '',
    body: r.respond && r.respond.body !== undefined ? JSON.stringify(r.respond.body, null, 2) : '',
    custom: !!(r.respond && r.respond.body !== undefined),
    headers: (r.respond && r.respond.headers) || {},
  }));
  const host = h('div', { class: 'stack-sm' });
  const slots = []; // per rule: {result, conds: [{key, el}]}
  let dragFrom = null;
  const listId = 'rule-names-' + Math.random().toString(36).slice(2, 8);
  const datalists = {};
  for (const k of Object.keys(params)) {
    datalists[k] = h('datalist', { id: listId + '-' + k }, ...params[k].map((n) => h('option', { value: n })));
  }

  function render() {
    clear(host);
    slots.length = 0;
    if (!list.length) {
      host.appendChild(h('p', { class: 'small muted', style: { margin: 0 } }, 'No rules. Add one to answer differently for specific requests, for example ', h('code', null, 'petId equals 0'), ' responds 404.'));
    }
    list.forEach((rule, i) => host.appendChild(ruleEl(rule, i)));
    if (!opts.readOnly) {
      host.appendChild(h('div', null, h('button', { class: 'btn sm', type: 'button', onclick: () => {
        const firstPath = params.path[0];
        list.push({ name: '', conds: [{ where: firstPath ? 'path' : 'query', name: firstPath || params.query[0] || '', op: 'equals', value: '' }], status: statuses.find((s) => s >= 400) || '', example: '', body: '', custom: false, headers: {} });
        render();
        onChange();
      } }, icon('plus'), 'Add rule')));
    }
    host.append(...Object.values(datalists));
    previewSoon();
  }

  function ruleEl(rule, i) {
    const nameIn = h('input', { class: 'input sm', placeholder: 'Name (optional)', value: rule.name, 'aria-label': `Rule ${i + 1} name` });
    nameIn.addEventListener('input', () => { rule.name = nameIn.value; onChange(); });
    const conds = h('div', { class: 'stack-sm' });
    const slot = { result: h('span', { class: 'rule-result', 'aria-live': 'polite' }), conds: [] };
    slots[i] = slot;
    rule.conds.forEach((c, j) => conds.appendChild(condEl(rule, c, j, i, slot)));
    const addCond = opts.readOnly ? null : h('button', { class: 'btn sm ghost', type: 'button', onclick: () => { rule.conds.push({ where: 'query', name: '', op: 'equals', value: '' }); render(); onChange(); } }, icon('plus'), 'and');
    // Respond with.
    const statusSel = h('select', { class: 'select sm', 'aria-label': `Rule ${i + 1} status` },
      h('option', { value: '' }, 'default status'), ...statuses.map((s) => h('option', { value: String(s) }, String(s))));
    if (rule.status && !statuses.includes(Number(rule.status))) statusSel.appendChild(h('option', { value: String(rule.status) }, String(rule.status)));
    statusSel.value = String(rule.status || '');
    const withSel = h('select', { class: 'select sm', 'aria-label': `Rule ${i + 1} body` });
    function fillWith() {
      clear(withSel);
      withSel.appendChild(h('option', { value: '' }, 'the mock\'s body for this status'));
      for (const ex of examplesFor(rule.status)) withSel.appendChild(h('option', { value: 'ex:' + ex }, 'example ' + ex));
      withSel.appendChild(h('option', { value: 'custom' }, 'a custom body'));
      withSel.value = rule.custom ? 'custom' : rule.example ? 'ex:' + rule.example : '';
      if (rule.example && withSel.value !== 'ex:' + rule.example) { withSel.appendChild(h('option', { value: 'ex:' + rule.example }, 'example ' + rule.example)); withSel.value = 'ex:' + rule.example; }
    }
    fillWith();
    const bodyIn = textarea({ rows: 4, placeholder: '{"code": 404, "message": "not found"}', 'aria-label': `Rule ${i + 1} custom body`, hidden: !rule.custom });
    bodyIn.value = rule.body;
    bodyIn.addEventListener('input', () => { rule.body = bodyIn.value; onChange(); });
    statusSel.addEventListener('change', () => { rule.status = statusSel.value ? Number(statusSel.value) : ''; if (!rule.custom) rule.example = ''; fillWith(); onChange(); });
    withSel.addEventListener('change', () => {
      rule.custom = withSel.value === 'custom';
      rule.example = withSel.value.startsWith('ex:') ? withSel.value.slice(3) : '';
      bodyIn.hidden = !rule.custom;
      onChange();
    });
    const grip = opts.readOnly ? null : h('span', { class: 'rule-grip', title: 'Drag to reorder', 'aria-hidden': 'true' }, icon('grip'));
    const tools = opts.readOnly ? null : h('div', { class: 'row nowrap', style: { gap: '2px' } },
      h('button', { class: 'btn sm icon ghost', type: 'button', title: 'Move up', 'aria-label': `Move rule ${i + 1} up`, disabled: i === 0, onclick: () => { [list[i - 1], list[i]] = [list[i], list[i - 1]]; render(); onChange(); } }, icon('up')),
      h('button', { class: 'btn sm icon ghost', type: 'button', title: 'Move down', 'aria-label': `Move rule ${i + 1} down`, disabled: i === list.length - 1, onclick: () => { [list[i + 1], list[i]] = [list[i], list[i + 1]]; render(); onChange(); } }, icon('down')),
      h('button', { class: 'btn sm icon ghost', type: 'button', title: 'Remove rule', 'aria-label': `Remove rule ${i + 1}`, onclick: () => { list.splice(i, 1); render(); onChange(); } }, icon('trash')));
    const el = h('div', { class: 'rule', 'data-rule': String(i) },
      h('div', { class: 'row nowrap' }, grip, h('span', { class: 'step-n' }, String(i + 1)), h('div', { class: 'grow' }, nameIn), slot.result, tools),
      conds, addCond ? h('div', null, addCond) : null,
      h('div', { class: 'rule-line' }, h('span', { class: 'rule-kw' }, 'respond'), statusSel, h('span', { class: 'rule-kw' }, 'with'), withSel),
      bodyIn);
    // Drag to reorder: only from the grip, so text in the inputs stays selectable.
    if (grip) {
      grip.addEventListener('pointerdown', () => { el.draggable = true; });
      el.addEventListener('dragstart', (e) => { dragFrom = i; el.classList.add('dragging'); e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', String(i)); });
      el.addEventListener('dragend', () => { el.draggable = false; el.classList.remove('dragging'); dragFrom = null; });
    }
    el.addEventListener('dragover', (e) => { if (dragFrom === null || dragFrom === i) return; e.preventDefault(); el.classList.add('drop-target'); });
    el.addEventListener('dragleave', () => el.classList.remove('drop-target'));
    el.addEventListener('drop', (e) => {
      e.preventDefault();
      el.classList.remove('drop-target');
      if (dragFrom === null || dragFrom === i) return;
      const [moved] = list.splice(dragFrom, 1);
      list.splice(i, 0, moved);
      dragFrom = null;
      render();
      onChange();
    });
    return el;
  }

  function condEl(rule, c, j, i, slot) {
    const whereSel = select(WHERE, c.where === 'body' ? 'body' : c.where, { 'aria-label': `Rule ${i + 1} condition ${j + 1} source`, class: 'select sm' });
    const nameIn = h('input', { class: 'input sm mono', value: c.name, placeholder: c.where === 'body' ? 'a.b (empty: whole body)' : 'name', 'aria-label': `Rule ${i + 1} condition ${j + 1} name`, list: datalists[c.where] ? listId + '-' + c.where : null });
    const opSel = select(OPERATORS, c.op, { 'aria-label': `Rule ${i + 1} condition ${j + 1} operator`, class: 'select sm' });
    const valIn = h('input', { class: 'input sm mono', value: c.value, placeholder: c.op === 'matches' ? '^[0-9]+$' : 'value', 'aria-label': `Rule ${i + 1} condition ${j + 1} value`, hidden: c.op === 'exists' || c.op === 'missing' });
    whereSel.addEventListener('change', () => {
      c.where = whereSel.value;
      if (datalists[c.where]) nameIn.setAttribute('list', listId + '-' + c.where); else nameIn.removeAttribute('list');
      nameIn.placeholder = c.where === 'body' ? 'a.b (empty: whole body)' : 'name';
      onChange();
    });
    nameIn.addEventListener('input', () => { c.name = nameIn.value.trim(); onChange(); });
    opSel.addEventListener('change', () => { c.op = opSel.value; valIn.hidden = c.op === 'exists' || c.op === 'missing'; onChange(); });
    valIn.addEventListener('input', () => { c.value = valIn.value; onChange(); });
    const del = opts.readOnly || rule.conds.length < 2 ? null : h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove condition', onclick: () => { rule.conds.splice(j, 1); render(); onChange(); } }, icon('x'));
    const seen = h('span', { class: 'cond-seen' });
    slot.conds.push({ cond: c, el: seen });
    return h('div', { class: 'rule-line' }, h('span', { class: 'rule-kw' }, j === 0 ? 'when' : 'and'), whereSel, nameIn, opSel, valIn, del, seen);
  }

  function condKey(c) {
    return c.where === 'body' ? (c.name ? 'body.' + c.name : 'body') : c.where + '.' + (c.where === 'header' ? c.name.toLowerCase() : c.name);
  }

  function get() {
    return list.map((rule, i) => {
      const when = {};
      for (const c of rule.conds) {
        if (c.where !== 'body' && !c.name) throw new Error(`Rule ${i + 1}: name the ${c.where === 'header' ? 'header' : c.where + ' parameter'} to check.`);
        const key = condKey(c);
        const cond = {};
        if (c.op === 'equals') cond.equals = c.value;
        else if (c.op === 'not_equals') cond.not_equals = c.value;
        else if (c.op === 'matches') cond.matches = c.value;
        else cond.exists = c.op === 'exists';
        when[key] = cond;
      }
      const respond = {};
      if (rule.status) respond.status = Number(rule.status);
      if (rule.custom) {
        const text = rule.body.trim();
        if (text) { try { respond.body = JSON.parse(text); } catch { respond.body = rule.body; } } else respond.body = '';
      } else if (rule.example) respond.example = rule.example;
      if (Object.keys(rule.headers || {}).length) respond.headers = rule.headers;
      const out = { when, respond };
      if (rule.name.trim()) out.name = rule.name.trim();
      return out;
    });
  }

  // ── "Would match" preview against a captured call ──
  const callSel = h('select', { class: 'select sm', 'aria-label': 'Captured call to test the rules against' });
  const previewMsg = h('div', { class: 'small', 'aria-live': 'polite' });
  let previewSeq = 0;
  const previewSoon = debounce(runPreview, 450);
  callSel.addEventListener('change', () => runPreview());
  const previewEl = h('div', { class: 'rule-preview stack-sm' },
    h('div', { class: 'row' }, h('span', { class: 'label' }, 'Would match'), h('div', { class: 'grow', style: { minWidth: '200px' } }, callSel),
      h('button', { class: 'btn sm icon ghost', type: 'button', title: 'Reload the captured calls', 'aria-label': 'Reload the captured calls', onclick: loadCalls }, icon('refresh'))),
    previewMsg);

  async function loadCalls() {
    const r = await api('GET', `/${encodeURIComponent(opts.name)}/requests?limit=200`);
    if (!alive) return;
    const keep = callSel.value;
    clear(callSel);
    callSel.appendChild(h('option', { value: '' }, 'the latest call of this operation'));
    const calls = r.ok && r.json ? (r.json.requests || []).filter((e) => e.operation === op.id).slice(0, 25) : [];
    for (const e of calls) callSel.appendChild(h('option', { value: String(e.seq) }, `#${e.seq} ${e.method} ${e.path}${e.query ? '?' + e.query : ''} (${e.status}, ${relTime(e.timestamp)})`));
    if (calls.some((e) => String(e.seq) === keep)) callSel.value = keep;
    runPreview();
  }

  function clearMarks() {
    for (const s of slots) {
      if (!s) continue;
      replace(s.result);
      for (const c of s.conds) replace(c.el);
    }
  }

  async function runPreview() {
    if (!alive) return;
    let rules;
    try { rules = get(); } catch (e) { clearMarks(); replace(previewMsg, h('span', { class: 'muted' }, 'Finish the rules to see the preview. ' + e.message)); return; }
    const seq = ++previewSeq;
    const body = { rules };
    if (callSel.value) body.request = Number(callSel.value);
    const r = await api('POST', `/${encodeURIComponent(opts.name)}/operations/${encodeURIComponent(op.id)}/rules/test`, body);
    if (!alive || seq !== previewSeq) return;
    clearMarks();
    if (!r.ok || !r.json) { replace(previewMsg, h('span', { class: 'err-text' }, errorText(r, 'The preview'))); return; }
    const res = r.json;
    if (!res.request) {
      replace(previewMsg, h('span', { class: 'muted' }, res.hint || 'No captured call of this operation yet: send one with Try it.',
        opts.onTry ? h('span', null, ' ', h('button', { class: 'linklike', type: 'button', onclick: () => opts.onTry(op.id) }, 'Open Try it')) : null));
      return;
    }
    const q = res.request;
    const against = h('span', { class: 'mono' }, `#${q.seq} ${q.method} ${q.path}${q.query ? '?' + q.query : ''}`);
    const win = res.winner === null || res.winner === undefined ? null : res.rules.find((x) => x.index === res.winner);
    const resp = res.respond || {};
    replace(previewMsg, win
      ? h('span', null, 'For ', against, ' (answered ', String(q.status), ', ', relTime(q.timestamp), '): ',
        h('strong', null, `rule ${win.index + 1}${win.name ? ' "' + win.name + '"' : ''} answers`), resp.status ? ` with ${resp.status}` : '', '.')
      : h('span', null, 'For ', against, ' (answered ', String(q.status), ', ', relTime(q.timestamp), '): ', h('strong', null, 'no rule matches'), ', the default response answers.'));
    for (const rr of res.rules || []) {
      const s = slots[rr.index];
      if (!s) continue;
      const winner = rr.index === res.winner;
      replace(s.result, winner ? badge(h('span', null, icon('check'), ' would answer'), 'ok') : rr.matched ? badge('matches, an earlier rule wins', 'info') : badge('no match', ''));
      for (const c of s.conds) {
        const hit = (rr.conditions || []).find((x) => x.key === condKey(c.cond));
        if (!hit) continue;
        replace(c.el, h('span', { class: 'cond-seen-v ' + (hit.matched ? 'ok' : 'no'), title: hit.matched ? 'condition holds' : 'condition does not hold' },
          icon(hit.matched ? 'check' : 'x'), hit.value === null || hit.value === undefined ? 'missing' : 'seen ' + JSON.stringify(hit.value)));
      }
    }
  }

  render();
  loadCalls();
  return { el: host, preview: previewEl, get, reload: loadCalls, destroy() { alive = false; } };
}
