// AI assist in the console: the status of the instance's provider
// (GET /_rustybin/assist), the assist bar in the top bar (Cmd+K / Ctrl+K)
// with the current view's suggestions, the drawer on the right that streams
// answers (POST /_rustybin/assist/run as SSE) with apply cards for the
// actions a task proposes (open_view, copy, run_request; later steps add
// theirs), the sparkle button views drop where the work is, and a small
// safe markdown renderer (DOM nodes, never innerHTML). Without a key every
// entry point is an invitation that opens Management, AI.
import { h, replace, clear, icon, notice, toast, copyText, badge, spinner, field, textarea } from './dom.js';
import { SERVER, send, getJson, sseParser, readText } from './http.js';
import * as auth from './auth.js';
import * as settings from './settings.js';

const HISTORY_KEY = 'rustybin.console.assist.history';
const DRAWER_KEY = 'rustybin.console.assist.drawer';
const MAX_HISTORY = 20;
const MAX_CONTEXT_CHARS = 24000;

let status = null;          // the last GET /_rustybin/assist body, or null
let statusPromise = null;
const listeners = new Set();
// The context document of the current view (set by views, read by the bar).
let currentView = 'overview';
let contextProvider = null;

function emit() {
  for (const fn of listeners) {
    try { fn(status); } catch (e) { console.error(e); }
  }
}

/** Subscribe to status changes. */
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** The last status (null before the first load). */
export function getStatus() {
  return status;
}

/** True when a provider and key are set (or the mock provider). */
export function enabled() {
  return !!(status && status.enabled);
}

/** Load (or reload) the status; resolves to it, or null when unavailable. */
export async function refresh() {
  if (auth.needsSignIn()) { status = null; emit(); return null; }
  if (statusPromise) return statusPromise;
  statusPromise = getJson('/_rustybin/assist', { session: false })
    .then((s) => { status = s; return s; })
    .catch((e) => { console.warn('assist status unavailable', e); status = null; return null; })
    .finally(() => { statusPromise = null; emit(); });
  return statusPromise;
}

/** Views tell the bar where they are and how to describe what they show. */
export function setView(id, provider) {
  currentView = id || 'overview';
  contextProvider = typeof provider === 'function' ? provider : null;
  renderSuggestions();
}

/** The current view's context document, trimmed to a sane size. */
function currentContext() {
  let ctx = null;
  try { ctx = contextProvider ? contextProvider() : null; } catch (e) { console.warn('assist context', e); }
  if (ctx === null || ctx === undefined) ctx = { status: settings.instance.status ? { version: settings.instance.status.version, healthy: settings.instance.status.healthy } : null };
  let text = typeof ctx === 'string' ? ctx : JSON.stringify(ctx);
  if (text.length > MAX_CONTEXT_CHARS) text = text.slice(0, MAX_CONTEXT_CHARS) + '\n[truncated by the console]';
  return typeof ctx === 'string' ? text : (text.length < MAX_CONTEXT_CHARS ? ctx : text);
}

/** Static suggestions of a view (from the status, else a short default). */
export function staticSuggestions(view) {
  const table = status && status.suggestions;
  const list = table && (table[view] || table.overview);
  return Array.isArray(list) ? list : [
    { label: 'Describe an API to mock', prompt: 'Describe an API I want to mock and tell me how to create it here.' },
    { label: 'What did the gateway send?', prompt: 'Explain the last request this instance captured and what the gateway added.' },
    { label: 'Write a load test', prompt: 'Write a harness scenario that sends 50 requests per second to /echo for a minute.' },
  ];
}

// ── Running tasks ──────────────────────────────────────────────────

/**
 * Run a task and stream its events. `on` has optional sent, delta, json,
 * action, usage, error, done handlers. Resolves when the stream ends; the
 * result is {text, json, actions, usage, sent, error}.
 */
export async function run(task, inputs, on = {}, opts = {}) {
  const result = { text: '', json: null, actions: [], usage: null, sent: null, error: null };
  const body = { task, inputs, stream: true };
  if (opts.model) body.model = opts.model;
  const r = await send({ method: 'POST', url: SERVER + '/_rustybin/assist/run', headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' }, body: JSON.stringify(body), session: false, stream: true, timeout: 120000, signal: opts.signal });
  if (r.error || !r.ok) {
    let detail = r.error;
    if (!detail && r.response) {
      try { const j = await r.response.json(); detail = j.error || ('HTTP ' + r.status); if (j.reason === 'rate_limited' && j.retry_after) detail += ' (' + j.retry_after + ' s)'; } catch { detail = 'HTTP ' + r.status; }
    }
    result.error = detail || ('HTTP ' + r.status);
    result.status = r.status;
    if (on.error) on.error(result.error, r.status);
    return result;
  }
  const feed = sseParser((ev) => {
    let data = null;
    try { data = JSON.parse(ev.data); } catch { data = null; }
    switch (ev.event) {
      case 'sent': result.sent = data; if (on.sent) on.sent(data); break;
      case 'delta': if (data && typeof data.text === 'string') { result.text += data.text; if (on.delta) on.delta(data.text, result.text); } break;
      case 'json': result.json = data; if (on.json) on.json(data); break;
      case 'action': if (data) { result.actions.push(data); if (on.action) on.action(data); } break;
      case 'usage': result.usage = data; if (on.usage) on.usage(data); break;
      case 'error': result.error = data && data.error ? data.error : 'the assistant failed'; if (on.error) on.error(result.error, data && data.status); break;
      case 'done': if (on.done) on.done(result); break;
      default: break;
    }
  });
  try {
    await readText(r.response, feed, opts.signal);
  } catch (e) {
    if (!(opts.signal && opts.signal.aborted)) {
      result.error = result.error || ('the stream ended early: ' + (e && e.message ? e.message : e));
      if (on.error) on.error(result.error);
    }
  }
  return result;
}

// ── Markdown (safe: DOM nodes only) ───────────────────────────────

function inline(text) {
  // `code`, **bold**, *italic*, and bare http(s) links.
  const out = [];
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(https?:\/\/[^\s)<>"']+)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1]) out.push(h('code', null, m[1].slice(1, -1)));
    else if (m[2]) out.push(h('strong', null, m[2].slice(2, -2)));
    else if (m[3]) out.push(h('em', null, m[3].slice(1, -1)));
    else if (m[4]) out.push(h('a', { href: m[4], target: '_blank', rel: 'noopener noreferrer' }, m[4]));
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/** Render markdown text to an element: headings, paragraphs, lists, code. Action blocks are left out. */
export function renderMarkdown(text) {
  const root = h('div', { class: 'md' });
  const lines = String(text || '').replace(/\r\n?/g, '\n').split('\n');
  let i = 0;
  let para = [];
  let list = null;
  const flushPara = () => { if (para.length) { root.appendChild(h('p', null, ...inline(para.join(' ')))); para = []; } };
  const flushList = () => { if (list) { root.appendChild(list.el); list = null; } };
  while (i < lines.length) {
    const line = lines[i];
    const fence = /^```\s*(\w+)?\s*$/.exec(line);
    if (fence) {
      flushPara(); flushList();
      const lang = (fence[1] || '').toLowerCase();
      const code = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { code.push(lines[i]); i++; }
      i++;
      if (lang === 'action') continue; // rendered as apply cards
      const body = code.join('\n');
      const pre = h('pre', { class: 'md-code' }, h('code', null, body));
      const bar = h('div', { class: 'md-code-bar' }, lang ? h('span', { class: 'muted small' }, lang) : h('span'), h('button', { class: 'btn sm ghost', type: 'button', onclick: () => copyText(body, 'Code') }, icon('copy'), 'Copy'));
      root.appendChild(h('div', { class: 'md-code-wrap' }, bar, pre));
      continue;
    }
    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (heading) {
      flushPara(); flushList();
      const level = Math.min(4, heading[1].length + 2);
      root.appendChild(h('h' + level, null, ...inline(heading[2])));
      i++;
      continue;
    }
    const item = /^\s*(?:[-*]|\d+[.)])\s+(.*)$/.exec(line);
    if (item) {
      flushPara();
      const ordered = /^\s*\d/.test(line);
      if (!list || list.ordered !== ordered) { flushList(); list = { ordered, el: h(ordered ? 'ol' : 'ul') }; }
      list.el.appendChild(h('li', null, ...inline(item[1])));
      i++;
      continue;
    }
    if (!line.trim()) { flushPara(); flushList(); i++; continue; }
    para.push(line.trim());
    i++;
  }
  flushPara(); flushList();
  return root;
}

// ── Apply cards ────────────────────────────────────────────────────

/** The card of one action: a label, what it does, an Apply button. */
export function applyCard(action) {
  const a = action || {};
  const p = a.payload || {};
  let title = a.label || a.action;
  let detail = '';
  let apply = null;
  if (a.action === 'open_view') {
    title = a.label || ('Open ' + p.view);
    detail = '#/' + p.view;
    apply = () => { location.hash = '#/' + p.view; toast('Opened ' + p.view); };
  } else if (a.action === 'copy') {
    title = a.label || 'Copy';
    detail = String(p.text || '').slice(0, 200);
    apply = () => copyText(String(p.text || ''), 'Text');
  } else if (a.action === 'run_request') {
    title = a.label || ('Send ' + p.method + ' ' + p.path);
    detail = p.method + ' ' + p.path;
    apply = async (card) => {
      const out = card.querySelector('.apply-result') || h('div', { class: 'apply-result' });
      replace(out, spinner('Sending'));
      card.appendChild(out);
      const body = p.body === undefined || p.body === null ? undefined : (typeof p.body === 'string' ? p.body : JSON.stringify(p.body));
      const headers = Object.entries(p.headers || {});
      if (body && typeof p.body !== 'string' && !headers.some(([k]) => k.toLowerCase() === 'content-type')) headers.push(['Content-Type', 'application/json']);
      const r = await send({ method: p.method, url: SERVER + p.path, headers, body, timeout: 30000 });
      if (r.error) { replace(out, notice('err', r.error)); return; }
      replace(out, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, badge(String(r.status), r.ok ? 'ok' : 'err'), h('span', { class: 'muted small' }, Math.round(r.ms) + ' ms')),
        h('pre', { class: 'md-code small' }, h('code', null, r.text.slice(0, 2000)))));
    };
  } else if (a.action === 'create_mock') {
    title = a.label || 'Create a mock from this description';
    detail = String(p.description || '').slice(0, 200);
    apply = () => {
      settings.remember('mocks.describe', { description: p.description, openapi_version: p.openapi_version });
      if (location.hash.startsWith('#/mocks')) document.dispatchEvent(new CustomEvent('rustybin-describe'));
      else location.hash = '#/mocks';
      toast('Opening the mock wizard');
    };
  } else if (a.action === 'edit_spec') {
    title = a.label || ('Edit ' + p.mock + ' with AI');
    detail = String(p.instruction || '').slice(0, 200);
    apply = () => {
      settings.remember('mocks.edit', { mock: p.mock, instruction: p.instruction });
      location.hash = '#/mocks/' + encodeURIComponent(p.mock);
      toast('Opening ' + p.mock);
    };
  } else if (a.action === 'generate_examples') {
    title = a.label || ('Generate examples for ' + p.mock);
    detail = '#/mocks/' + p.mock;
    apply = () => {
      settings.remember('mocks.generate', { mock: p.mock });
      location.hash = '#/mocks/' + encodeURIComponent(p.mock);
      toast('Opening ' + p.mock);
    };
  } else {
    return notice('', 'An action this console cannot apply yet: ' + a.action);
  }
  const card = h('div', { class: 'apply-card' },
    h('div', { class: 'row' }, icon('ai'), h('div', { class: 'grow' }, h('div', { class: 'apply-title' }, title), detail ? h('div', { class: 'muted small mono ellipsis' }, detail) : null),
      h('button', { class: 'btn sm primary', type: 'button', onclick: () => apply(card) }, 'Apply')));
  return card;
}

// ── The drawer ─────────────────────────────────────────────────────

let drawerEl = null;
let drawerBody = null;
let drawerOpen = false;
let history = [];
let abort = null;
let bodyEl = null;

try { history = JSON.parse(sessionStorage.getItem(HISTORY_KEY) || '[]'); } catch { history = []; }
if (!Array.isArray(history)) history = [];

function saveHistory() {
  history = history.slice(-MAX_HISTORY);
  try { sessionStorage.setItem(HISTORY_KEY, JSON.stringify(history)); } catch { /* ignore */ }
}

function exchangeEl(x) {
  const answer = h('div', { class: 'assist-answer' });
  if (x.text) answer.appendChild(renderMarkdown(x.text));
  if (x.json) answer.appendChild(h('pre', { class: 'md-code' }, h('code', null, JSON.stringify(x.json, null, 2))));
  for (const a of x.actions || []) answer.appendChild(applyCard(a));
  if (x.error) answer.appendChild(notice('err', x.error));
  return h('div', { class: 'assist-exchange', 'data-id': x.id },
    h('div', { class: 'assist-question' }, icon('user'), h('div', { class: 'grow' }, x.question), x.view ? h('span', { class: 'badge outline' }, x.view) : null),
    answer,
    footer(x));
}

function footer(x) {
  const u = x.usage;
  const parts = [];
  if (u) {
    parts.push(u.provider + (u.model ? ' / ' + u.model : ''));
    if (u.cached) parts.push('cached');
    else parts.push((u.input_tokens || 0) + ' in, ' + (u.output_tokens || 0) + ' out');
    if (typeof u.estimated_cost_usd === 'number') parts.push('about $' + u.estimated_cost_usd.toFixed(4));
  }
  const sent = x.sent ? h('details', { class: 'disclosure small' }, h('summary', null, 'What was sent'),
    h('pre', { class: 'md-code small' }, h('code', null, JSON.stringify({ provider: x.sent.provider, model: x.sent.model, task: x.sent.task, system: x.sent.system, user: x.sent.user, schema: x.sent.schema }, null, 2)))) : null;
  return h('div', { class: 'assist-foot' }, parts.length ? h('span', { class: 'muted small' }, parts.join(' · ')) : null, sent);
}

function renderDrawerBody() {
  if (!bodyEl) return;
  clear(bodyEl);
  if (!history.length) {
    bodyEl.appendChild(h('div', { class: 'assist-empty' }, icon('ai'),
      h('p', null, enabled() ? 'Ask anything about this instance, or pick a suggestion. Answers can open views, copy text and send requests; nothing is applied without a click.' : 'Add an AI key under Management, AI to ask questions here. The built-in mock provider works without one.')));
    return;
  }
  for (const x of history) bodyEl.appendChild(exchangeEl(x));
  bodyEl.scrollTop = bodyEl.scrollHeight;
}

function ensureDrawer() {
  if (drawerEl) return drawerEl;
  bodyEl = h('div', { class: 'assist-body' });
  const closeBtn = h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close the assistant', onclick: closeDrawer }, icon('x'));
  const clearBtn = h('button', { class: 'btn sm ghost', type: 'button', id: 'assist-clear', onclick: () => { history = []; saveHistory(); renderDrawerBody(); } }, 'Clear');
  const grip = h('div', { class: 'assist-grip', title: 'Drag to resize' });
  drawerEl = h('aside', { class: 'assist-drawer', id: 'assist-drawer', role: 'complementary', 'aria-label': 'Assistant', hidden: true },
    grip,
    h('div', { class: 'assist-head' }, icon('ai'), h('strong', { class: 'grow' }, 'Assistant'), clearBtn, closeBtn),
    bodyEl);
  document.body.appendChild(drawerEl);
  let w = 0;
  try { w = Number(localStorage.getItem(DRAWER_KEY + '.width')) || 0; } catch { w = 0; }
  if (w) drawerEl.style.width = Math.min(Math.max(w, 320), window.innerWidth - 80) + 'px';
  grip.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = drawerEl.getBoundingClientRect().width;
    const move = (ev) => { const nw = Math.min(Math.max(startW + (startX - ev.clientX), 320), window.innerWidth - 80); drawerEl.style.width = nw + 'px'; };
    const up = () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up); try { localStorage.setItem(DRAWER_KEY + '.width', String(parseInt(drawerEl.style.width, 10) || 0)); } catch { /* ignore */ } };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  });
  renderDrawerBody();
  return drawerEl;
}

export function openDrawer() {
  ensureDrawer();
  drawerOpen = true;
  drawerEl.hidden = false;
  document.body.classList.add('assist-open');
  try { sessionStorage.setItem(DRAWER_KEY, '1'); } catch { /* ignore */ }
}

export function closeDrawer() {
  if (!drawerEl) return;
  drawerOpen = false;
  drawerEl.hidden = true;
  document.body.classList.remove('assist-open');
  try { sessionStorage.removeItem(DRAWER_KEY); } catch { /* ignore */ }
}

export function isDrawerOpen() {
  return drawerOpen;
}

/** Ask a question from the current view: streams the answer into the drawer. */
export async function ask(question, opts = {}) {
  const q = String(question || '').trim();
  if (!q) return;
  if (!enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI to ask questions'); return; }
  openDrawer();
  if (abort) abort.abort();
  abort = new AbortController();
  const x = { id: 'x' + Date.now().toString(36), question: q, view: opts.view || currentView, text: '', json: null, actions: [], usage: null, sent: null, error: null, at: new Date().toISOString() };
  history.push(x);
  saveHistory();
  renderDrawerBody();
  const el = bodyEl.querySelector('[data-id="' + x.id + '"]');
  const answer = el.querySelector('.assist-answer');
  replace(answer, spinner('Thinking'));
  const md = h('div');
  let streaming = false;
  const inputs = { question: q, view: x.view, context: opts.context !== undefined ? opts.context : currentContext() };
  await run(opts.task || 'free_answer', inputs, {
    delta: (_piece, all) => {
      if (!streaming) { replace(answer, md); streaming = true; }
      x.text = all;
      replace(md, renderMarkdown(all));
      bodyEl.scrollTop = bodyEl.scrollHeight;
    },
    json: (v) => { x.json = v; replace(answer, h('pre', { class: 'md-code' }, h('code', null, JSON.stringify(v, null, 2)))); },
    action: (a) => { x.actions.push(a); answer.appendChild(applyCard(a)); },
    usage: (u) => { x.usage = u; },
    sent: (s) => { x.sent = s; },
    error: (e) => { x.error = e; if (!streaming) clear(answer); answer.appendChild(notice('err', e)); },
  }, { signal: abort.signal, model: opts.model });
  saveHistory();
  const foot = el.querySelector('.assist-foot');
  if (foot) foot.replaceWith(footer(x));
  bodyEl.scrollTop = bodyEl.scrollHeight;
}

// ── The bar ────────────────────────────────────────────────────────

let barEl = null;
let suggestEl = null;
let inputEl = null;

function renderSuggestions() {
  if (!suggestEl) return;
  clear(suggestEl);
  const list = staticSuggestions(currentView);
  for (const s of list.slice(0, 4)) {
    suggestEl.appendChild(h('button', { class: 'btn sm ghost assist-suggestion', type: 'button', onclick: () => { inputEl.value = s.prompt; submit(); } }, s.label));
  }
  if (enabled() && status && status.provider !== 'mock') {
    // Ask the model for better ones (cached server side for an hour per view).
    run('suggest', { view: currentView }, {
      json: (v) => {
        if (!v || !Array.isArray(v.suggestions)) return;
        clear(suggestEl);
        for (const s of v.suggestions.slice(0, 4)) suggestEl.appendChild(h('button', { class: 'btn sm ghost assist-suggestion', type: 'button', onclick: () => { inputEl.value = s.prompt; submit(); } }, s.label));
      },
    }).catch(() => {});
  }
}

function submit() {
  const q = inputEl.value.trim();
  if (!q) return;
  inputEl.value = '';
  closePopover();
  ask(q);
}

let popEl = null;
function openPopover() {
  if (!popEl) return;
  popEl.hidden = false;
  renderSuggestions();
}
function closePopover() {
  if (popEl) popEl.hidden = true;
}

/** Mount the assist bar into `host` (the top bar). */
export function mountBar(host) {
  if (barEl) return barEl;
  inputEl = h('input', { class: 'input assist-input', type: 'text', placeholder: 'Ask Rustybin (Ctrl+K)', 'aria-label': 'Ask Rustybin', autocomplete: 'off', id: 'assist-input' });
  suggestEl = h('div', { class: 'assist-suggestions', id: 'assist-suggestions' });
  const hint = h('div', { class: 'muted small assist-hint', id: 'assist-hint' });
  popEl = h('div', { class: 'assist-pop', id: 'assist-pop', hidden: true }, hint, suggestEl);
  const form = h('form', { class: 'assist-form', id: 'assist-bar' }, icon('ai'), inputEl, h('button', { class: 'btn sm primary', type: 'submit', 'aria-label': 'Ask' }, icon('send')));
  form.addEventListener('submit', (e) => { e.preventDefault(); submit(); });
  inputEl.addEventListener('focus', openPopover);
  inputEl.addEventListener('keydown', (e) => { if (e.key === 'Escape') { closePopover(); inputEl.blur(); } });
  barEl = h('div', { class: 'assist-wrap', id: 'assist-wrap' }, form, popEl);
  document.addEventListener('click', (e) => { if (!barEl.contains(e.target)) closePopover(); });
  host.appendChild(barEl);
  const paint = () => {
    const on = enabled();
    barEl.classList.toggle('off', !on);
    inputEl.placeholder = on ? 'Ask Rustybin (Ctrl+K)' : 'Add an AI key to describe what you want';
    const st = status || {};
    replace(hint, on
      ? h('span', null, 'Answers from ', h('strong', null, st.provider + (st.model ? ' / ' + st.model : '')), st.provider === 'mock' ? ' (the built-in mock: nothing leaves this instance)' : '', '. Suggestions for this view:')
      : h('span', null, (st.reason || 'No AI provider is set') + '. ', h('a', { href: '#/manage/ai' }, 'Open Management, AI'), '. The built-in mock LLM needs no key.'));
    renderSuggestions();
  };
  onChange(paint);
  paint();
  return barEl;
}

/** Focus the bar (Cmd+K / Ctrl+K). */
export function focusBar() {
  if (!inputEl) return;
  if (!enabled()) { location.hash = '#/manage/ai'; return; }
  inputEl.focus();
  inputEl.select();
}

/**
 * A sparkle button views drop where the work is. `task` and `inputs` (a
 * value or a function) describe the call; the answer goes to the drawer as
 * a question labelled `label`. Without a key it points at Management, AI.
 */
export function assistButton(label, task, inputs, opts = {}) {
  const on = enabled();
  const b = h('button', { class: 'btn sm ' + (opts.class || '') + (on ? '' : ' assist-off'), type: 'button', title: on ? label : 'Add an AI key in Management, AI', 'data-assist': task }, icon('ai'), label);
  b.addEventListener('click', () => {
    if (!enabled()) { location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; }
    const value = typeof inputs === 'function' ? inputs() : inputs;
    if (task === 'free_answer') { ask(value.question || label, { context: value.context, view: value.view }); return; }
    askTask(task, value, label);
  });
  return b;
}

/** Run a structured task into the drawer, shown under `label`. */
export async function askTask(task, inputs, label) {
  openDrawer();
  if (abort) abort.abort();
  abort = new AbortController();
  const x = { id: 'x' + Date.now().toString(36), question: label, view: currentView, text: '', json: null, actions: [], usage: null, sent: null, error: null, at: new Date().toISOString() };
  history.push(x);
  saveHistory();
  renderDrawerBody();
  const el = bodyEl.querySelector('[data-id="' + x.id + '"]');
  const answer = el.querySelector('.assist-answer');
  replace(answer, spinner('Thinking'));
  await run(task, inputs, {
    json: (v) => { x.json = v; replace(answer, structured(task, v)); },
    delta: (_p, all) => { x.text = all; replace(answer, renderMarkdown(all)); },
    action: (a) => { x.actions.push(a); answer.appendChild(applyCard(a)); },
    usage: (u) => { x.usage = u; },
    sent: (s) => { x.sent = s; },
    error: (e) => { x.error = e; clear(answer); answer.appendChild(notice('err', e)); },
  }, { signal: abort.signal });
  saveHistory();
  const foot = el.querySelector('.assist-foot');
  if (foot) foot.replaceWith(footer(x));
}

/** A readable rendering of a structured answer (explain, suggest). */
function structured(task, v) {
  if (task === 'explain' && v && typeof v === 'object') {
    const el = h('div', { class: 'md' });
    if (v.summary) el.appendChild(h('p', null, h('strong', null, v.summary)));
    const section = (title, items) => { if (Array.isArray(items) && items.length) { el.appendChild(h('h4', null, title)); el.appendChild(h('ul', null, ...items.map((s) => h('li', null, String(s))))); } };
    section('Details', v.details);
    section('What the gateway added', v.gateway_notes);
    section('Next steps', v.next_steps);
    return el;
  }
  if (task === 'suggest' && v && Array.isArray(v.suggestions)) {
    return h('div', { class: 'pill-list' }, ...v.suggestions.map((s) => h('button', { class: 'btn sm', type: 'button', onclick: () => ask(s.prompt) }, s.label)));
  }
  return h('pre', { class: 'md-code' }, h('code', null, JSON.stringify(v, null, 2)));
}

/** The "Ask Rustybin" card of the overview: three suggestions, or the invitation. */
export function askCard() {
  const body = h('div', { class: 'stack-sm' });
  const paint = () => {
    clear(body);
    if (!enabled()) {
      const st = status || {};
      body.appendChild(notice('', h('div', { class: 'grow' }, (st.reason || 'No AI provider is set') + '. ', h('a', { href: '#/manage/ai' }, 'Add a key under Management, AI'), ' to describe what you want to mock or test; the built-in mock provider needs none.')));
      return;
    }
    body.appendChild(h('div', { class: 'pill-list' }, ...staticSuggestions('overview').slice(0, 3).map((s) => h('button', { class: 'btn sm', type: 'button', onclick: () => ask(s.prompt, { view: 'overview' }) }, icon('ai'), s.label))));
    const ta = textarea({ rows: 2, placeholder: 'Describe what you want to mock or test', 'aria-label': 'Ask Rustybin from the overview', id: 'overview-ask' });
    const form = h('form', { class: 'stack-sm' }, field('Or ask', ta), h('div', { class: 'row' }, h('span', { class: 'grow' }), h('button', { class: 'btn sm primary', type: 'submit' }, icon('send'), 'Ask')));
    form.addEventListener('submit', (e) => { e.preventDefault(); const q = ta.value.trim(); if (q) { ta.value = ''; ask(q, { view: 'overview' }); } });
    body.appendChild(form);
  };
  const off = onChange(paint);
  paint();
  const el = h('div', null, body);
  el.destroy = off;
  return el;
}

// Reopen the drawer where the tab left it.
try { if (sessionStorage.getItem(DRAWER_KEY) === '1' && history.length) setTimeout(openDrawer, 0); } catch { /* ignore */ }
