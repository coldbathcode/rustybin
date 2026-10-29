// AI on mocks: the wizard's Describe tab (a spec from a description), the
// example coverage banner with Generate (AI) and Use generated values, the
// Generate examples dialog with a preview per operation, and Edit with AI
// (an instruction, a streamed diff, Apply as draft). Every call streams
// progress from the mock AI routes (POST /_rustybin/mocks/ai/spec,
// /{name}/ai/examples, /{name}/ai/edit); without a key the entry points
// invite to Management, AI, and "Use generated values" works regardless.
import { h, replace, clear, icon, notice, spinner, badge, field, input, select, textarea, toast, methodBadge } from '../../lib/dom.js';
import { diffView } from '../../lib/format.js';
import { SERVER, send, sseParser, readText } from '../../lib/http.js';
import * as assist from '../../lib/assist.js';
import { plural } from '../../lib/mockapi.js';

const API = '/_rustybin/mocks';
const enc = encodeURIComponent;

export const VERSIONS = [['3.1.0', 'OpenAPI 3.1.0 (default)'], ['3.0.3', 'OpenAPI 3.0.3'], ['2.0', 'Swagger 2.0 (legacy)']];
export const STYLE = {
  auth: [['none', 'No auth'], ['api_key', 'API key'], ['bearer', 'Bearer token'], ['oauth2', 'OAuth2 client credentials']],
  pagination: [['none', 'No pagination'], ['page', 'Page / page_size'], ['cursor', 'Cursor']],
  errors: [['problem', 'problem+json errors'], ['simple', 'Simple {code, message}']],
};
export const PLACEHOLDER = 'An orders API for a coffee shop: customers, products, orders with line items and a payment status; API key auth; cursor pagination.';

/**
 * POST a mock AI route and stream its events. `on`: progress(message,
 * data), usage(data), result(data), error(message, data). Resolves to
 * {ok, result, error, status}.
 */
export async function aiCall(path, body, on = {}, opts = {}) {
  const out = { ok: false, result: null, error: null, status: 0 };
  const r = await send({ method: 'POST', url: SERVER + API + path, headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' }, body: JSON.stringify(Object.assign({ stream: true }, body)), session: false, admin: true, stream: true, timeout: 180000, signal: opts.signal });
  out.status = r.status;
  if (r.error || !r.ok) {
    let detail = r.error;
    if (!detail && r.response) {
      try { const j = await r.response.json(); detail = j.error || ('HTTP ' + r.status); if (j.retry_after) detail += ' (' + j.retry_after + ' s)'; out.json = j; } catch { detail = 'HTTP ' + r.status; }
    }
    out.error = detail || ('HTTP ' + r.status);
    if (on.error) on.error(out.error, out.json);
    return out;
  }
  const feed = sseParser((ev) => {
    let data = null;
    try { data = JSON.parse(ev.data); } catch { data = null; }
    if (ev.event === 'progress' && data) { if (on.progress) on.progress(data.message, data); }
    else if (ev.event === 'usage') { if (on.usage) on.usage(data); }
    else if (ev.event === 'result') { out.ok = true; out.result = data; if (on.result) on.result(data); }
    else if (ev.event === 'error') { out.error = (data && data.error) || 'the assistant failed'; out.json = data; if (on.error) on.error(out.error, data); }
  });
  try {
    await readText(r.response, feed, opts.signal);
  } catch (e) {
    if (!(opts.signal && opts.signal.aborted)) { out.error = out.error || ('the stream ended early: ' + (e && e.message ? e.message : e)); if (on.error) on.error(out.error); }
  }
  return out;
}

/** A progress line that logs the streamed messages. */
export function progressLog() {
  const list = h('ul', { class: 'small muted ai-progress' });
  const el = h('div', { class: 'stack-sm' }, h('div', { class: 'row nowrap' }, h('span', { class: 'spinner' }), h('span', { class: 'small', id: 'ai-progress-current' }, 'Starting')), list);
  return {
    el,
    say(message) {
      const cur = el.querySelector('#ai-progress-current');
      if (cur) cur.textContent = message;
      list.appendChild(h('li', null, message));
      if (list.children.length > 12) list.removeChild(list.firstChild);
    },
    done() { const sp = el.querySelector('.spinner'); if (sp) sp.remove(); },
  };
}

function usageLine(u) {
  if (!u) return null;
  const bits = [u.provider + (u.model ? ' / ' + u.model : '')];
  if (!u.cached) bits.push((u.input_tokens || 0) + ' in, ' + (u.output_tokens || 0) + ' out');
  if (typeof u.estimated_cost_usd === 'number') bits.push('about $' + u.estimated_cost_usd.toFixed(4));
  return h('span', { class: 'muted small' }, bits.join(' | '));
}

function invitation(what) {
  const st = assist.getStatus() || {};
  return notice('', icon('ai'), h('div', { class: 'grow' }, h('strong', null, 'AI assist is off. '), (st.reason || 'No AI provider is set') + '. ', h('a', { href: '#/manage/ai' }, 'Add a key under Management, AI'), ' to ' + what + '; the built-in mock provider works without one.'));
}

// ── The Describe tab of the wizard ─────────────────────────────────

/**
 * The body of the wizard's Describe tab. `opts.onSpec(text, preview)` gets
 * the generated document; `opts.initial` prefills the prompt and
 * `opts.autorun` starts at once (an apply card from the assist bar).
 */
export function describeTab(opts = {}) {
  const prompt = textarea({ rows: 6, placeholder: PLACEHOLDER, 'aria-label': 'Describe the API', id: 'describe-prompt' });
  if (opts.initial) prompt.value = opts.initial;
  const version = select(VERSIONS, opts.version || '3.1.0', { 'aria-label': 'OpenAPI version', id: 'describe-version' });
  const chips = {};
  const chipRow = (name) => {
    const row = h('div', { class: 'pill-list', role: 'radiogroup', 'aria-label': name });
    chips[name] = STYLE[name][0][0];
    for (const [value, label] of STYLE[name]) {
      const b = h('button', { class: 'btn sm' + (value === chips[name] ? ' primary' : ''), type: 'button', role: 'radio', 'aria-checked': String(value === chips[name]), 'data-style': name + ':' + value }, label);
      b.addEventListener('click', () => {
        chips[name] = value;
        for (const x of row.children) { const on = x.dataset.style === name + ':' + value; x.classList.toggle('primary', on); x.setAttribute('aria-checked', String(on)); }
      });
      row.appendChild(b);
    }
    return row;
  };
  const out = h('div');
  const go = h('button', { class: 'btn primary', type: 'button', id: 'describe-generate' }, icon('ai'), 'Generate');
  const el = h('div', { class: 'stack' });
  let abort = null;
  async function run() {
    const text = prompt.value.trim();
    if (!text) { replace(out, notice('warn', 'Describe the API first: what it is for, its resources, how clients authenticate.')); prompt.focus(); return; }
    if (!assist.enabled()) { replace(out, invitation('describe an API')); return; }
    go.disabled = true;
    if (abort) abort.abort();
    abort = new AbortController();
    const log = progressLog();
    replace(out, log.el);
    let usage = null;
    const r = await aiCall('/ai/spec', { description: text, openapi_version: version.value, style: { auth: chips.auth, pagination: chips.pagination, errors: chips.errors } }, {
      progress: (m) => log.say(m),
      usage: (u) => { usage = u; },
    }, { signal: abort.signal });
    go.disabled = false;
    log.done();
    if (!r.ok) { replace(out, notice('err', h('strong', null, 'No usable document came back. '), r.error || '')); return; }
    const p = r.result.preview || {};
    replace(out, notice('ok', h('div', { class: 'grow' }, h('strong', null, (p.title || 'The API') + ': '), plural((p.operations || []).length, 'operation'), ', checks ' + (p.report && p.report.ok ? 'ok' : (p.report ? p.report.errors + ' error(s)' : '?')),
      p.coverage ? ', examples on ' + p.coverage.with_examples + ' of ' + p.coverage.operations + ' operations' : '', '. ', usageLine(usage || r.result.usage))));
    if (opts.onSpec) opts.onSpec(r.result.text, p, r.result);
  }
  go.addEventListener('click', run);
  prompt.addEventListener('keydown', (e) => { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run(); } });
  el.append(
    assist.enabled() ? null : invitation('describe an API and get a mock'),
    field('Describe the API', prompt, 'What it is for, its resources and their fields, who may call it. Ctrl+Enter generates.'),
    h('div', { class: 'grid grid-2' },
      field('OpenAPI version', version, 'The document conforms to it; 2.0 is converted to 3.0 when the mock is created.'),
      h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Style'), chipRow('auth'), chipRow('pagination'), chipRow('errors'))),
    h('div', { class: 'row' }, h('span', { class: 'muted small' }, 'The document is checked before it is offered; nothing is created until you say so.'), h('span', { class: 'grow' }), go),
    out);
  const api = { el, run, focus: () => prompt.focus(), destroy: () => { if (abort) abort.abort(); } };
  if (opts.autorun && opts.initial) setTimeout(run, 0);
  return api;
}

// ── Coverage banner ────────────────────────────────────────────────

/**
 * The banner of a mock whose operations lack examples. `ctx`: {name,
 * detail(), refresh(), readOnly}. Returns null when complete.
 */
export function coverageBanner(ctx) {
  const d = ctx.detail();
  const cov = d && d.coverage;
  if (!cov || cov.complete) return null;
  const without = cov.without || [];
  const n = without.length;
  const details = h('details', { class: 'disclosure small' }, h('summary', null, 'Which ones'),
    h('ul', { class: 'issue-list small' }, ...without.slice(0, 20).map((o) => h('li', null, methodBadge(o.method), ' ', h('span', { class: 'mono' }, o.path), h('span', { class: 'muted' }, ' (' + o.id + ', status ' + o.statuses.join(', ') + ')'))),
      n > 20 ? h('li', { class: 'muted' }, 'and ' + (n - 20) + ' more') : null));
  const generateBtn = assist.assistButton('Generate with AI', 'examples_for_operations', () => ({}), { class: 'primary', id: 'coverage-generate-ai' });
  // The sparkle button's own click opens the drawer; here it opens the dialog instead.
  generateBtn.onclick = (e) => { e.stopImmediatePropagation(); if (!assist.enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; } openExamplesDialog(ctx, 'ai'); };
  generateBtn.id = 'coverage-generate-ai';
  const generatedBtn = h('button', { class: 'btn sm', type: 'button', id: 'coverage-generate-values', onclick: () => openExamplesDialog(ctx, 'generated') }, 'Use generated values');
  return notice('warn', icon('info'), h('div', { class: 'grow' },
    h('strong', null, `${n} of ${cov.operations} ${plural(cov.operations, 'operation').replace(/^\d+ /, '')} ${n === 1 ? 'has' : 'have'} no example for a success response. `),
    'Calls answer with data generated from the schema; the MCP tools of this mock return the same. Give them examples:',
    h('div', { class: 'row', style: { marginTop: '6px' } }, ctx.readOnly ? null : generateBtn, ctx.readOnly ? null : generatedBtn, details)));
}

// ── Generate examples dialog ───────────────────────────────────────

function modal(title, id) {
  const dlg = h('dialog', { class: 'wizard', id, 'aria-labelledby': id + '-title' });
  const body = h('div', { class: 'wizard-body stack' });
  const foot = h('div', { class: 'wizard-foot row' });
  dlg.append(h('div', { class: 'wizard-head row nowrap' }, h('h2', { id: id + '-title', class: 'grow' }, title),
    h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, icon('x'))), body, foot);
  dlg.addEventListener('close', () => dlg.remove());
  document.body.appendChild(dlg);
  dlg.showModal();
  return { dlg, body, foot };
}

/**
 * Generate examples for the operations without one (or `only` ids), with
 * `source` 'ai' (a persona, the configured model) or 'generated' (the
 * deterministic generator); preview per operation, then Save to draft.
 */
export function openExamplesDialog(ctx, source, only) {
  const { dlg, body, foot } = modal(source === 'ai' ? 'Generate examples with AI' : 'Use generated values', 'examples-dialog');
  const d = ctx.detail();
  const persona = input({ value: 'realistic data for a demo', 'aria-label': 'Persona', id: 'examples-persona', placeholder: 'realistic retail data, edge cases, ...' });
  const per = select([['1', '1 per response'], ['2', '2 per response'], ['3', '3 per response']], '1', { 'aria-label': 'Examples per response', id: 'examples-per' });
  const out = h('div');
  const targets = only || ((d.coverage && d.coverage.without) || []).map((o) => o.id);
  let result = null;
  let abort = null;
  const genBtn = h('button', { class: 'btn primary', type: 'button', id: 'examples-run' }, icon(source === 'ai' ? 'ai' : 'refresh'), source === 'ai' ? 'Generate' : 'Generate values');
  const saveBtn = h('button', { class: 'btn primary', type: 'button', id: 'examples-save', disabled: true }, icon('check'), 'Save to draft');
  async function run(apply) {
    genBtn.disabled = true;
    saveBtn.disabled = true;
    if (abort) abort.abort();
    abort = new AbortController();
    const log = progressLog();
    replace(out, log.el);
    const r = await aiCall('/' + enc(ctx.name) + '/ai/examples', { source, operations: only || undefined, per_response: Number(per.value), persona: source === 'ai' ? persona.value.trim() : undefined, apply }, { progress: (m) => log.say(m) }, { signal: abort.signal });
    genBtn.disabled = false;
    log.done();
    if (!dlg.open) return;
    if (!r.ok) { replace(out, notice('err', h('strong', null, 'No examples came back. '), r.error || '')); return; }
    result = r.result;
    if (apply) {
      toast(`${result.stored} example(s) saved to the draft`);
      dlg.close();
      ctx.refresh();
      return;
    }
    const examples = result.examples || {};
    const ops = Object.keys(examples);
    const rows = ops.map((id) => {
      const byStatus = examples[id] || {};
      return h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('strong', { class: 'mono' }, id), ...Object.keys(byStatus).map((s) => badge(s + ': ' + plural((byStatus[s] || []).length, 'example'), 'info'))),
        ...Object.entries(byStatus).map(([s, list]) => h('pre', { class: 'code small', style: { maxHeight: '160px' } }, JSON.stringify(list.length === 1 ? list[0] : list, null, 2))));
    });
    replace(out,
      notice('ok', h('div', { class: 'grow' }, h('strong', null, `${plural(ops.length, 'operation')} covered. `), 'Every example was validated against its response schema. ', usageLine(result.usage))),
      h('div', { class: 'stack', id: 'examples-preview' }, ...rows));
    saveBtn.disabled = !ops.length;
  }
  genBtn.addEventListener('click', () => run(false));
  saveBtn.addEventListener('click', () => run(true));
  body.append(
    notice('', h('div', { class: 'grow' }, `${plural(targets.length, 'operation')} ${targets.length === 1 ? 'needs' : 'need'} examples: `, h('span', { class: 'mono small' }, targets.slice(0, 12).join(', ') + (targets.length > 12 ? ', ...' : '')), '.',
      source === 'ai' ? ' The configured model writes them from the response schemas; each one is validated before it is shown.' : ' The deterministic generator fills them from the schemas (the same values the mock serves today, now stored and editable).')),
    source === 'ai' ? field('Style of the data', persona) : null,
    field('How many', per),
    out);
  foot.append(h('span', { class: 'muted small' }, 'Saved examples appear in the draft with a badge; deploy to serve them.'), h('span', { class: 'grow' }),
    h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'), genBtn, saveBtn);
  genBtn.focus();
  return dlg;
}

// ── Edit with AI ───────────────────────────────────────────────────

/** The Edit with AI dialog: an instruction, a streamed diff, Apply as draft. */
export function openEditDialog(ctx, initialInstruction) {
  const { dlg, body, foot } = modal('Edit the spec with AI', 'edit-dialog');
  const instruction = textarea({ rows: 3, placeholder: 'add a DELETE for orders and a 404 example', 'aria-label': 'Instruction', id: 'edit-instruction' });
  if (initialInstruction) instruction.value = initialInstruction;
  const out = h('div');
  let result = null;
  let abort = null;
  const runBtn = h('button', { class: 'btn primary', type: 'button', id: 'edit-run' }, icon('ai'), 'Show the diff');
  const applyBtn = h('button', { class: 'btn primary', type: 'button', id: 'edit-apply', disabled: true }, icon('check'), 'Apply as draft');
  async function run(apply) {
    const text = instruction.value.trim();
    if (!text) { replace(out, notice('warn', 'Say what should change.')); instruction.focus(); return; }
    if (!assist.enabled()) { replace(out, invitation('edit the spec')); return; }
    runBtn.disabled = true;
    applyBtn.disabled = true;
    if (abort) abort.abort();
    abort = new AbortController();
    const log = progressLog();
    replace(out, log.el);
    const r = await aiCall('/' + enc(ctx.name) + '/ai/edit', { instruction: text, apply }, { progress: (m) => log.say(m) }, { signal: abort.signal });
    runBtn.disabled = false;
    log.done();
    if (!dlg.open) return;
    if (!r.ok) { replace(out, notice('err', h('strong', null, 'No usable edit came back. '), r.error || '')); return; }
    result = r.result;
    if (apply) {
      toast('The edited spec is the draft; deploy to publish it');
      dlg.close();
      ctx.refresh();
      return;
    }
    const diff = result.diff || {};
    const p = result.preview || {};
    const summary = [];
    if ((diff.added || []).length) summary.push(plural(diff.added.length, 'operation') + ' added (' + diff.added.join(', ') + ')');
    if ((diff.removed || []).length) summary.push(plural(diff.removed.length, 'operation') + ' removed (' + diff.removed.join(', ') + ')');
    if ((diff.changed || []).length) summary.push(plural(diff.changed.length, 'operation') + ' changed');
    if (diff.info_changed) summary.push('info changed');
    if (diff.components_changed) summary.push('schemas changed');
    replace(out,
      notice(p.report && p.report.ok ? 'ok' : 'warn', h('div', { class: 'grow' }, h('strong', null, summary.join('; ') || 'No structural change'), '. Checks of the new version: ' + (p.report ? p.report.errors + ' error(s), ' + p.report.warnings + ' warning(s)' : '?') + '. ', usageLine(result.usage))),
      h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Diff (before, after)'), h('div', { id: 'edit-diff' }, diffView(result.before_text || '', result.text || ''))));
    applyBtn.disabled = false;
  }
  runBtn.addEventListener('click', () => run(false));
  applyBtn.addEventListener('click', () => run(true));
  instruction.addEventListener('keydown', (e) => { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run(false); } });
  body.append(
    assist.enabled() ? null : invitation('edit the spec with an instruction'),
    field('Instruction', instruction, 'The whole document comes back edited; you see the diff and the deploy checks before anything is saved. Ctrl+Enter shows the diff.'),
    out);
  foot.append(h('span', { class: 'muted small' }, 'Apply saves the new spec as the draft; deploy publishes it.'), h('span', { class: 'grow' }),
    h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'), runBtn, applyBtn);
  instruction.focus();
  if (initialInstruction) setTimeout(() => run(false), 0);
  return dlg;
}

/** The small list of an operation's stored examples (the editor). */
export function storedExamplesList(examples, status) {
  const list = (examples && examples[String(status)]) || [];
  if (!list.length) return null;
  return h('div', { class: 'stack-sm', id: 'stored-examples' },
    h('span', { class: 'label' }, 'Stored examples for ' + status),
    ...list.map((e) => h('details', { class: 'disclosure small' }, h('summary', null, icon(e.source === 'ai' ? 'ai' : e.source === 'generated' ? 'refresh' : 'user'), ' ', e.name, ' ', badge(e.source, e.source === 'ai' ? 'violet' : 'outline')),
      h('pre', { class: 'code small', style: { maxHeight: '160px' } }, JSON.stringify(e.value, null, 2)))));
}

/** Clear the DOM helpers the module does not use itself (linters). */
export const _unused = { clear };
