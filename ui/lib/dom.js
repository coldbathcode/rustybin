// DOM helpers and small UI components. Everything is built with
// createElement/textContent: user or server data is never parsed as HTML.

/** h('div', {class: 'x', onclick: fn}, child, 'text', [more]) */
export function h(tag, props, ...children) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else if (k === 'value') el.value = v;
      else if (k === 'checked') el.checked = !!v;
      else if (k === 'dataset') Object.assign(el.dataset, v);
      else if (v === true) el.setAttribute(k, '');
      else el.setAttribute(k, String(v));
    }
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else if (c instanceof Node) el.appendChild(c);
    else el.appendChild(document.createTextNode(String(c)));
  }
}

export function clear(el) {
  while (el.firstChild) el.removeChild(el.firstChild);
  return el;
}

export function replace(el, ...children) {
  clear(el);
  append(el, children);
  return el;
}

const SVG_NS = 'http://www.w3.org/2000/svg';

/** SVG element builder (charts, icons). */
export function s(tag, attrs, ...children) {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null) continue;
    if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, String(v));
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    el.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

// Feather-style line icons (24x24, stroke).
const ICONS = {
  overview: 'M3 13h8V3H3zM13 21h8V11h-8zM13 3v6h8V3zM3 21h8v-6H3z',
  traffic: 'M22 12h-4l-3 9L9 3l-3 9H2',
  bin: 'M21 8v13H3V8M1 3h22v5H1zM10 12h4',
  explorer: 'M4 6h16M4 12h16M4 18h10',
  ai: 'M12 2l2.4 5.6L20 10l-5.6 2.4L12 18l-2.4-5.6L4 10l5.6-2.4zM19 17l.9 2.1L22 20l-2.1.9L19 23l-.9-2.1L16 20l2.1-.9z',
  mcp: 'M9 2v6M15 2v6M6 8h12v4a6 6 0 0 1-12 0zM12 18v4',
  a2a: 'M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8M23 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75',
  chaos: 'M13 2L3 14h9l-1 8 10-12h-9z',
  token: 'M21 2l-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.78 7.78 5.5 5.5 0 0 1 7.78-7.78zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4',
  copy: 'M9 9h13v13H9zM5 15H2V2h13v3',
  check: 'M20 6L9 17l-5-5',
  sun: 'M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10zM12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42',
  moon: 'M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z',
  menu: 'M3 12h18M3 6h18M3 18h18',
  pause: 'M6 4h4v16H6zM14 4h4v16h-4z',
  play: 'M5 3l14 9-14 9z',
  trash: 'M3 6h18M19 6l-1 14H6L5 6M10 11v6M14 11v6M9 6V3h6v3',
  send: 'M22 2L11 13M22 2l-7 20-4-9-9-4z',
  terminal: 'M4 17l6-6-6-6M12 19h8',
  diff: 'M12 3v18M3 8h6M6 5v6M15 16h6',
  external: 'M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14L21 3',
  info: 'M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 16v-4M12 8h.01',
  alert: 'M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0zM12 9v4M12 17h.01',
  refresh: 'M23 4v6h-6M1 20v-6h6M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15',
  plus: 'M12 5v14M5 12h14',
  x: 'M18 6L6 18M6 6l12 12',
  stop: 'M6 6h12v12H6z',
  link: 'M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71',
  inbox: 'M22 12h-6l-2 3h-4l-2-3H2M5.45 5.11L2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z',
  shield: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  settings: 'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z',
  globe: 'M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z',
  user: 'M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z',
};

export function icon(name, cls) {
  const path = ICONS[name] || ICONS.info;
  return s('svg', { viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': 2, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true', class: cls },
    s('path', { d: path }));
}

// ── Feedback ───────────────────────────────────────────────────────

let toastHost;
export function toast(message, kind) {
  if (!toastHost) {
    toastHost = h('div', { class: 'toasts', role: 'status', 'aria-live': 'polite' });
    document.body.appendChild(toastHost);
  }
  const t = h('div', { class: 'toast' + (kind === 'err' ? ' err' : '') }, message);
  toastHost.appendChild(t);
  setTimeout(() => t.remove(), kind === 'err' ? 5000 : 2200);
}

/** Copy text; falls back to a hidden textarea outside secure contexts. */
export async function copyText(text, label) {
  let ok = false;
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      ok = true;
    }
  } catch { ok = false; }
  if (!ok) {
    const ta = h('textarea', { style: { position: 'fixed', top: '-1000px', opacity: '0' } });
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { ok = document.execCommand('copy'); } catch { ok = false; }
    ta.remove();
  }
  toast(ok ? (label || 'Copied to clipboard') : 'Copy failed: select the text and copy it manually', ok ? undefined : 'err');
  return ok;
}

export function copyButton(getText, opts = {}) {
  const b = h('button', { class: 'btn sm' + (opts.icon ? ' icon' : ''), type: 'button', title: opts.title || 'Copy', 'aria-label': opts.title || 'Copy' },
    icon('copy'), opts.icon ? null : (opts.label || 'Copy'));
  b.addEventListener('click', (e) => {
    e.stopPropagation();
    const t = typeof getText === 'function' ? getText() : getText;
    copyText(t, opts.toast);
  });
  return b;
}

export function notice(kind, ...content) {
  const ic = kind === 'err' ? 'alert' : kind === 'warn' ? 'alert' : kind === 'ok' ? 'check' : 'info';
  return h('div', { class: 'notice ' + (kind || ''), role: kind === 'err' ? 'alert' : null }, icon(ic), h('div', { class: 'grow' }, ...content));
}

export function empty(title, text, iconName) {
  return h('div', { class: 'empty' }, iconName ? icon(iconName) : null, h('h3', null, title), text ? h('p', null, text) : null);
}

export function spinner(label) {
  return h('span', { class: 'row', role: 'status' }, h('span', { class: 'spinner' }), label ? h('span', { class: 'muted small' }, label) : null);
}

// ── Inputs ─────────────────────────────────────────────────────────

export function field(label, control, hint) {
  return h('label', { class: 'field' }, h('span', null, label), control, hint ? h('span', { class: 'hint' }, hint) : null);
}

export function input(props) {
  return h('input', Object.assign({ class: 'input', type: 'text', spellcheck: 'false', autocomplete: 'off' }, props));
}

export function textarea(props) {
  return h('textarea', Object.assign({ class: 'textarea mono', spellcheck: 'false' }, props));
}

export function select(options, value, props) {
  const el = h('select', Object.assign({ class: 'select' }, props));
  for (const o of options) {
    const [v, label] = Array.isArray(o) ? o : [o, o];
    const opt = h('option', { value: v }, label);
    if (v === value) opt.selected = true;
    el.appendChild(opt);
  }
  return el;
}

export function checkbox(label, checked, onchange) {
  const cb = h('input', { type: 'checkbox', checked, onchange: (e) => onchange && onchange(e.target.checked) });
  return { el: h('label', { class: 'check' }, cb, label), input: cb };
}

/** Segmented control. options: [[value, label]]. Returns {el, get, set}. */
export function segmented(options, value, onchange, ariaLabel) {
  let current = value;
  const el = h('div', { class: 'seg', role: 'group', 'aria-label': ariaLabel || null });
  const buttons = options.map(([v, label]) => {
    const b = h('button', { type: 'button', 'aria-pressed': String(v === current) }, label);
    b.addEventListener('click', () => { set(v); if (onchange) onchange(v); });
    el.appendChild(b);
    return [v, b];
  });
  function set(v) {
    current = v;
    for (const [bv, b] of buttons) b.setAttribute('aria-pressed', String(bv === v));
  }
  return { el, get: () => current, set };
}

/** Tabs: [{id, label, render: () => Node}] -> {el, select(id)} */
export function tabs(items, initial, onchange) {
  const bar = h('div', { class: 'tabs', role: 'tablist' });
  const body = h('div', { role: 'tabpanel' });
  const el = h('div', null, bar, body);
  let current = null;
  const buttons = new Map();
  for (const it of items) {
    const b = h('button', { type: 'button', role: 'tab', 'aria-selected': 'false' }, it.label);
    b.addEventListener('click', () => selectTab(it.id));
    b.addEventListener('keydown', (e) => {
      if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
      const idx = items.findIndex((x) => x.id === current);
      const next = items[(idx + (e.key === 'ArrowRight' ? 1 : items.length - 1)) % items.length];
      selectTab(next.id);
      buttons.get(next.id).focus();
    });
    buttons.set(it.id, b);
    bar.appendChild(b);
  }
  function selectTab(id) {
    current = id;
    for (const [bid, b] of buttons) {
      b.setAttribute('aria-selected', String(bid === id));
      b.tabIndex = bid === id ? 0 : -1;
    }
    const it = items.find((x) => x.id === id) || items[0];
    replace(body, it.render());
    if (onchange) onchange(id);
  }
  selectTab(initial || items[0].id);
  return { el, select: selectTab, get: () => current };
}

// ── Display ────────────────────────────────────────────────────────

export function methodBadge(m) {
  const up = String(m || '').toUpperCase();
  return h('span', { class: 'm m-' + up }, up);
}

export function statusText(code) {
  const c = Number(code) || 0;
  return h('span', { class: 'st st-' + Math.floor(c / 100) }, c ? String(c) : 'ERR');
}

export function statusBadge(code, text) {
  const c = Number(code) || 0;
  const cls = c >= 500 || c === 0 ? 'err' : c >= 400 ? 'warn' : c >= 300 ? 'info' : 'ok';
  return h('span', { class: 'badge ' + cls }, c ? String(c) : 'ERR', text ? ' ' + text : null);
}

export function badge(text, kind) {
  return h('span', { class: 'badge ' + (kind || '') }, text);
}

export function card(title, opts, ...body) {
  opts = opts || {};
  return h('section', { class: 'card ' + (opts.class || '') },
    title !== null ? h('div', { class: 'card-head' }, h('h2', null, title), opts.hint ? h('span', { class: 'hint' }, opts.hint) : null, opts.actions || null) : null,
    h('div', { class: 'card-body' + (opts.flush ? ' flush' : '') }, ...body));
}

export function kvgrid(pairs) {
  const dl = h('dl', { class: 'kvgrid' });
  for (const [k, v] of pairs) {
    if (v === undefined) continue;
    dl.appendChild(h('dt', null, k));
    dl.appendChild(h('dd', null, v === null ? 'none' : v));
  }
  return dl;
}

export function fmtMs(ms) {
  if (ms === null || ms === undefined || Number.isNaN(ms)) return '';
  if (ms < 1) return ms.toFixed(2) + ' ms';
  if (ms < 1000) return Math.round(ms) + ' ms';
  return (ms / 1000).toFixed(2) + ' s';
}

export function fmtBytes(n) {
  if (!n) return '0 B';
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
  return (n / 1024 / 1024).toFixed(1) + ' MB';
}

export function fmtDuration(sec) {
  sec = Math.max(0, Math.floor(sec));
  const d = Math.floor(sec / 86400), hh = Math.floor((sec % 86400) / 3600), mm = Math.floor((sec % 3600) / 60), ss = sec % 60;
  if (d) return `${d}d ${hh}h ${mm}m`;
  if (hh) return `${hh}h ${mm}m ${ss}s`;
  if (mm) return `${mm}m ${ss}s`;
  return `${ss}s`;
}

export function fmtTime(iso) {
  const d = iso ? new Date(iso) : new Date();
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString([], { hour12: false }) + '.' + String(d.getMilliseconds()).padStart(3, '0');
}

export function debounce(fn, ms) {
  let t;
  return (...args) => { clearTimeout(t); t = setTimeout(() => fn(...args), ms); };
}

/** Random id from crypto.getRandomValues (works outside secure contexts). */
export function randomId(prefix, bytes = 6) {
  const b = new Uint8Array(bytes);
  crypto.getRandomValues(b);
  return (prefix || '') + Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}
