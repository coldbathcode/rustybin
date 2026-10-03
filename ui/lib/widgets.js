// Shared console widgets: the "direct or via gateway" target picker and the
// response viewer used by every request-sending view.
import { h, replace, segmented, statusBadge, fmtMs, fmtBytes, tabs, notice, copyButton, icon } from './dom.js';
import { bodyView, headersTable, copyableCode } from './format.js';
import * as settings from './settings.js';
import { SERVER, baseFor, toCurl, trimBase, isCrossOrigin } from './http.js';

/** Direct / via gateway selector. Returns {el, base(), via()}. */
export function targetPicker(key, onchange) {
  const memKey = 'target.' + key;
  let via = settings.recall(memKey, false) && !!settings.get('gatewayUrl');
  const label = h('span', { class: 'muted small mono ellipsis', style: { maxWidth: '360px' } });
  const seg = segmented([['direct', 'Direct'], ['gateway', 'Via gateway']], via ? 'gateway' : 'direct', (v) => {
    if (v === 'gateway' && !trimBase(settings.get('gatewayUrl'))) {
      seg.set('direct');
      document.getElementById('settings-btn')?.click();
      return;
    }
    via = v === 'gateway';
    settings.remember(memKey, via);
    refresh();
    if (onchange) onchange(via);
  }, 'Send requests directly or through the gateway');
  function refresh() {
    label.textContent = baseFor(via);
    label.title = via ? 'Requests go through the gateway URL from the console settings' : 'Requests go straight to this Rustybin instance';
  }
  refresh();
  const off = settings.onChange((k) => {
    if (k !== 'gatewayUrl') return;
    if (!trimBase(settings.get('gatewayUrl'))) { via = false; seg.set('direct'); }
    refresh();
  });
  const el = h('div', { class: 'row', style: { gap: '8px' } }, h('span', { class: 'label' }, 'Target'), seg.el, label);
  return { el, base: () => baseFor(via), via: () => via, destroy: off };
}

/** Render a send() result: status line, body, headers, request and curl. */
export function responseView(result, opts = {}) {
  const req = result.request || {};
  const curl = toCurl({ method: req.method, url: req.url, headers: req.headers, body: req.body });
  const head = h('div', { class: 'row', style: { padding: '10px 14px', borderBottom: '1px solid var(--border)' } },
    result.error ? statusBadge(0, 'no response') : statusBadge(result.status, result.statusText),
    h('span', { class: 'muted small' }, fmtMs(result.ms)),
    result.error ? null : h('span', { class: 'muted small' }, fmtBytes(new Blob([result.text || '']).size)),
    h('span', { class: 'muted small mono ellipsis grow', title: req.url }, (req.method || '') + ' ' + (req.url || '')),
    copyButton(curl, { label: 'Copy as curl', toast: 'curl command copied' }),
    opts.extraActions || null);
  const items = [];
  if (result.error) {
    items.push({ id: 'error', label: 'Error', render: () => h('div', { class: 'card-body stack-sm' }, notice('err', result.error),
      isCrossOrigin(req.url || '') ? h('p', { class: 'small muted' }, 'Tip: the same request sent with curl is not subject to CORS.') : null,
      copyableCode(curl)) });
  } else {
    items.push({ id: 'body', label: 'Body', render: () => h('div', { class: 'card-body' }, bodyView(result.text, result.contentType, { cls: 'tall' })) });
    items.push({ id: 'headers', label: `Headers (${result.headers.length})`, render: () => h('div', { class: 'card-body flush' }, headersTable(result.headers, { noHighlight: true })) });
  }
  items.push({ id: 'request', label: 'Request', render: () => h('div', { class: 'card-body stack-sm' },
    headersTable(req.headers || []), req.body ? bodyView(String(req.body), headerValue(req.headers, 'content-type')) : null) });
  items.push({ id: 'curl', label: 'curl', render: () => h('div', { class: 'card-body' }, copyableCode(curl)) });
  const t = tabs(items, opts.initialTab && items.some((i) => i.id === opts.initialTab) ? opts.initialTab : items[0].id);
  return h('div', { class: 'card' }, head, t.el);
}

export function headerValue(headers, name) {
  const n = name.toLowerCase();
  const hit = (headers || []).find(([k]) => String(k).toLowerCase() === n);
  return hit ? hit[1] : '';
}

/** A container that shows a spinner while `promise` runs, then its result. */
export function pending(el, label) {
  replace(el, h('div', { class: 'row muted small', style: { padding: '12px' } }, h('span', { class: 'spinner' }), label || 'Working'));
}

/** Simple editable header rows: returns {el, get(): [[k,v]], set(list)}. */
export function headerEditor(initial) {
  const rows = h('div', { class: 'stack-sm' });
  function addRow(k = '', v = '') {
    const kIn = h('input', { class: 'input sm mono', placeholder: 'Header', value: k, 'aria-label': 'Header name' });
    const vIn = h('input', { class: 'input sm mono', placeholder: 'Value', value: v, 'aria-label': 'Header value' });
    const del = h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove header', onclick: () => row.remove() }, icon('x'));
    const row = h('div', { class: 'row nowrap' }, h('div', { style: { flex: '0 0 38%' } }, kIn), h('div', { class: 'grow' }, vIn), del);
    rows.appendChild(row);
  }
  for (const [k, v] of initial || []) addRow(k, v);
  const add = h('button', { class: 'btn sm', type: 'button', onclick: () => addRow() }, icon('plus'), 'Add header');
  return {
    el: h('div', { class: 'stack-sm' }, rows, h('div', null, add)),
    get: () => Array.from(rows.children).map((r) => {
      const [a, b] = r.querySelectorAll('input');
      return [a.value.trim(), b.value];
    }).filter(([k]) => k),
    set: (list) => { replace(rows); for (const [k, v] of list || []) addRow(k, v); },
  };
}

export { SERVER };
