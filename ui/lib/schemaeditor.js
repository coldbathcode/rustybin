// Nested form built from a JSON Schema in the OpenAPI dialect (the mock API
// inlines $refs, so no resolver is needed). Objects are collapsible groups,
// arrays get add / remove rows, enums become selects, booleans toggles,
// oneOf / anyOf a variant picker; formats get suitable inputs (date, uuid with
// a "New" button, ...). Validation issues from the server (JSON pointers) are
// pinned to the matching field with showIssues().
import { h, replace, clear, icon } from './dom.js';
import { uuid } from './mockapi.js';

const MAX_DEPTH = 14;

function clone(v) {
  return v === undefined ? undefined : JSON.parse(JSON.stringify(v));
}

/** JSON pointer token for an object key. */
function esc(key) {
  return String(key).replace(/~/g, '~0').replace(/\//g, '~1');
}

const MERGED = ['type', 'enum', 'const', 'format', 'items', 'additionalProperties', 'oneOf', 'anyOf', 'minimum', 'maximum',
  'minLength', 'maxLength', 'pattern', 'minItems', 'maxItems', 'nullable', 'example', 'default', 'readOnly', 'writeOnly', 'discriminator'];

/** Flatten allOf into one schema (properties and required are merged). */
export function normalise(schema) {
  if (!schema || typeof schema !== 'object') return {};
  if (!Array.isArray(schema.allOf) || !schema.allOf.length) return schema;
  const out = Object.assign({}, schema);
  delete out.allOf;
  // Properties in allOf order; the schema's own properties win.
  const own = schema.properties;
  delete out.properties;
  for (const part of schema.allOf) {
    const p = normalise(part);
    if (p.properties) out.properties = Object.assign({}, out.properties || {}, p.properties);
    if (p.required) out.required = Array.from(new Set([...(out.required || []), ...p.required]));
    if (!out.description && p.description) out.description = p.description;
    for (const k of MERGED) if (out[k] === undefined && p[k] !== undefined) out[k] = p[k];
  }
  if (own) out.properties = Object.assign({}, out.properties || {}, own);
  if (!out.type && out.properties) out.type = 'object';
  return out;
}

/** Main type of a schema ('object', 'array', 'string', ... or null for any). */
export function schemaType(s) {
  let t = s.type;
  if (Array.isArray(t)) t = t.find((x) => x !== 'null') || t[0];
  if (t) return t;
  if (s.properties || s.additionalProperties) return 'object';
  if (s.items) return 'array';
  if (Array.isArray(s.enum) && s.enum.length) {
    const v = s.enum[0];
    return typeof v === 'number' ? 'number' : typeof v === 'boolean' ? 'boolean' : 'string';
  }
  return null;
}

function typeLabel(s) {
  if (s['x-recursive']) return 'recursive';
  const variants = s.oneOf || s.anyOf;
  if (variants) return (s.oneOf ? 'one of ' : 'any of ') + variants.length;
  const t = schemaType(s);
  if (!t) return 'any';
  if (t === 'array') {
    const it = normalise(s.items || {});
    return 'array of ' + (it.title || schemaType(it) || 'any');
  }
  return (s.title && t === 'object' ? s.title : t) + (s.format ? ' (' + s.format + ')' : '');
}

function today() {
  return new Date().toISOString().slice(0, 10);
}

function nowIso() {
  return new Date().toISOString().replace(/\.\d{3}Z$/, 'Z');
}

function formatSample(format) {
  switch (format) {
    case 'date': return today();
    case 'date-time': return nowIso();
    case 'uuid': return uuid();
    case 'email': return 'user@example.com';
    case 'uri': case 'url': return 'https://example.com';
    default: return '';
  }
}

/** A starting value for a schema: default, example, enum, then by type. */
export function blank(schema, depth = 0) {
  const s = normalise(schema);
  if (depth > 6) return null;
  if (s.default !== undefined) return clone(s.default);
  if (s.example !== undefined) return clone(s.example);
  if (s.const !== undefined) return clone(s.const);
  if (Array.isArray(s.enum) && s.enum.length) return clone(s.enum[0]);
  const variants = s.oneOf || s.anyOf;
  if (variants && variants.length) return blank(variants[0], depth + 1);
  switch (schemaType(s)) {
    case 'object': {
      const o = {};
      for (const r of s.required || []) if (s.properties && s.properties[r]) o[r] = blank(s.properties[r], depth + 1);
      return o;
    }
    case 'array': return [];
    case 'integer': case 'number': return s.minimum !== undefined ? s.minimum : 0;
    case 'boolean': return false;
    case 'string': return formatSample(s.format);
    default: return null;
  }
}

function constraints(s) {
  const parts = [];
  if (s.minimum !== undefined) parts.push('min ' + s.minimum);
  if (s.maximum !== undefined) parts.push('max ' + s.maximum);
  if (s.minLength !== undefined) parts.push('at least ' + s.minLength + ' chars');
  if (s.maxLength !== undefined) parts.push('at most ' + s.maxLength + ' chars');
  if (s.pattern) parts.push('pattern ' + s.pattern);
  if (s.minItems !== undefined) parts.push('at least ' + s.minItems + ' items');
  if (s.maxItems !== undefined) parts.push('at most ' + s.maxItems + ' items');
  if (s.readOnly) parts.push('read-only');
  return parts.join(', ');
}

/** Score how well a value fits a variant (to preselect oneOf / anyOf). */
function fit(s0, v) {
  const s = normalise(s0);
  const t = schemaType(s);
  if (v === undefined || v === null) return 0;
  if (Array.isArray(s.enum)) return s.enum.some((e) => JSON.stringify(e) === JSON.stringify(v)) ? 3 : 0;
  if (t === 'object') {
    if (typeof v !== 'object' || Array.isArray(v)) return 0;
    const props = s.properties || {};
    let score = 1;
    for (const k of Object.keys(v)) score += k in props ? 1 : -0.5;
    const disc = s.discriminator && s.discriminator.propertyName;
    if (disc && props[disc] && Array.isArray(props[disc].enum) && props[disc].enum.includes(v[disc])) score += 10;
    return score;
  }
  if (t === 'array') return Array.isArray(v) ? 2 : 0;
  if (t === 'integer') return Number.isInteger(v) ? 2 : 0;
  if (t === 'number') return typeof v === 'number' ? 2 : 0;
  if (t === 'boolean') return typeof v === 'boolean' ? 2 : 0;
  if (t === 'string') return typeof v === 'string' ? 2 : 0;
  return 1;
}

/**
 * Build the editor. opts: {onChange(value), direction: 'response' | 'request',
 * readOnly}. Returns {el, get(), set(value), showIssues(issues) -> unmatched}.
 */
export function schemaEditor(schema, value, opts = {}) {
  const fields = new Map();
  const root = h('div', { class: 'se' });
  let top = null;
  const ctx = {
    pointer: '',
    depth: 0,
    required: true,
    direction: opts.direction || 'response',
    disabled: !!opts.readOnly,
    fields,
    emit: () => { if (opts.onChange && top) opts.onChange(top.get()); },
  };
  function render(v) {
    fields.clear();
    const s = normalise(schema);
    const composite = ['object', 'array'].includes(schemaType(s)) || s.oneOf || s.anyOf;
    if (composite) {
      top = node(s, v, ctx);
      fields.set('', { wrap: root, msg: null });
      replace(root, top.el);
    } else {
      // A scalar body still gets a label and an issue slot.
      const msg = h('div', { class: 'se-msg', hidden: true });
      top = node(s, v, ctx);
      const wrap = h('div', { class: 'se-field' }, h('span', { class: 'se-label' }, 'Body', h('span', { class: 'se-type' }, typeLabel(s))), top.el, msg);
      fields.set('', { wrap, msg });
      replace(root, wrap);
    }
  }
  render(value);
  return {
    el: root,
    get: () => top.get(),
    set: (v) => render(v),
    /** Pin issues ([{pointer, message}]) to fields; returns the ones without a field. */
    showIssues(issues) {
      for (const f of fields.values()) {
        f.wrap.classList.remove('has-issue');
        if (f.msg) { f.msg.hidden = true; clear(f.msg); }
      }
      const rest = [];
      for (const is of issues || []) {
        let p = is.pointer === '/' ? '' : String(is.pointer || '');
        const m = /missing required property "([^"]+)"/.exec(is.message || '');
        if (m && fields.has(p + '/' + esc(m[1]))) p = p + '/' + esc(m[1]);
        // Walk up to the closest field that exists.
        let target = p;
        while (target && !(fields.has(target) && fields.get(target).wrap.isConnected)) target = target.slice(0, Math.max(0, target.lastIndexOf('/')));
        const f = fields.get(target);
        if (f && f.msg && f.wrap.isConnected) {
          f.wrap.classList.add('has-issue');
          f.msg.hidden = false;
          f.msg.appendChild(h('div', null, icon('alert'), h('span', null, (target !== p ? p.slice(target.length + 1) + ': ' : '') + is.message)));
        } else {
          rest.push(is);
        }
      }
      return rest;
    },
  };
}

function node(schema, value, ctx) {
  const s = normalise(schema);
  if (ctx.depth > MAX_DEPTH || s['x-recursive']) return anyNode(s, value, ctx);
  if ((s.oneOf || s.anyOf) && !s.properties) return variantNode(s, value, ctx);
  const t = schemaType(s);
  if (t === 'object') return objectNode(s, value, ctx);
  if (t === 'array') return arrayNode(s, value, ctx);
  if (t === null) return anyNode(s, value, ctx);
  return scalarNode(s, value, ctx, t);
}

function child(ctx, pointer, required) {
  return Object.assign({}, ctx, { pointer, required, depth: ctx.depth + 1 });
}

// ── Objects ────────────────────────────────────────────────────────

function objectNode(s, value, ctx) {
  const obj = value && typeof value === 'object' && !Array.isArray(value) ? value : {};
  const props = s.properties || {};
  const required = new Set(s.required || []);
  const box = h('div', { class: 'se-obj' });
  const kids = [];
  for (const [name, ps0] of Object.entries(props)) {
    const ps = normalise(ps0);
    if (ctx.direction === 'response' && ps.writeOnly) continue;
    if (ctx.direction === 'request' && ps.readOnly) continue;
    const f = propField(name, ps, obj[name], required.has(name), ctx);
    kids.push(f);
    box.appendChild(f.el);
  }
  // Keys outside `properties`: a map editor (additionalProperties), else kept as is.
  const known = new Set(Object.keys(props));
  const extra = Object.fromEntries(Object.entries(obj).filter(([k]) => !known.has(k)));
  let map = null;
  if (s.additionalProperties || (!Object.keys(props).length && s.additionalProperties !== false)) {
    map = mapEditor(s.additionalProperties && typeof s.additionalProperties === 'object' ? s.additionalProperties : {}, extra, ctx);
    box.appendChild(map.el);
  }
  if (!kids.length && !map) box.appendChild(h('p', { class: 'tiny muted' }, 'No properties.'));
  return {
    el: box,
    get() {
      const out = map ? {} : Object.assign({}, extra);
      for (const k of kids) {
        const v = k.get();
        if (v !== undefined) out[k.name] = v;
      }
      if (map) Object.assign(out, map.get());
      return out;
    },
  };
}

function propField(name, ps, value, required, ctx) {
  const pointer = ctx.pointer + '/' + esc(name);
  const t = schemaType(ps);
  const composite = t === 'object' || t === 'array' || ((ps.oneOf || ps.anyOf) && !ps.properties) || t === null;
  const msg = h('div', { class: 'se-msg', hidden: true });
  const hint = [ps.description, constraints(ps)].filter(Boolean).join(' | ');
  const label = h('span', { class: 'se-label' }, h('span', { class: 'mono' }, name),
    required ? h('span', { class: 'se-req', title: 'required' }, '*') : null,
    h('span', { class: 'se-type' }, typeLabel(ps)));
  const sub = child(ctx, pointer, required);
  if (!composite) {
    const inner = node(ps, value, sub);
    const wrap = h('div', { class: 'se-field' }, h('label', { class: 'se-head' }, label), inner.el, hint ? h('span', { class: 'hint' }, hint) : null, msg);
    const ctl = inner.el.matches('input, select, textarea') ? inner.el : inner.el.querySelector('input, select, textarea');
    if (ctl) ctl.setAttribute('aria-label', name);
    ctx.fields.set(pointer, { wrap, msg });
    return { name, el: wrap, get: inner.get };
  }
  // Objects and arrays: a collapsible group; optional ones can be added / removed.
  let present = required || value !== undefined;
  let inner = null;
  const slot = h('div', { class: 'se-body' });
  const toggleBtn = required || ctx.disabled ? null : h('button', { class: 'btn sm ghost', type: 'button' });
  const details = h('details', { class: 'se-group', open: ctx.depth < 3 });
  const summary = h('summary', null, label, h('span', { class: 'grow' }), toggleBtn);
  details.append(summary, hint ? h('span', { class: 'hint' }, hint) : null, msg, slot);
  function mount(v) {
    present = true;
    inner = node(ps, v, sub);
    replace(slot, inner.el);
    details.classList.remove('se-absent');
    if (toggleBtn) replace(toggleBtn, icon('x'), 'Remove');
  }
  function unmount() {
    present = false;
    inner = null;
    replace(slot, h('p', { class: 'tiny muted' }, 'Not included in the body.'));
    details.classList.add('se-absent');
    if (toggleBtn) replace(toggleBtn, icon('plus'), 'Add');
  }
  if (toggleBtn) {
    toggleBtn.addEventListener('click', (e) => {
      e.preventDefault();
      if (present) unmount();
      else { mount(blank(ps)); details.open = true; }
      ctx.emit();
    });
  }
  if (present) mount(value === undefined ? blank(ps) : value);
  else unmount();
  ctx.fields.set(pointer, { wrap: details, msg });
  return { name, el: details, get: () => (present && inner ? inner.get() : undefined) };
}

function mapEditor(valueSchema, value, ctx) {
  const rows = h('div', { class: 'stack-sm' });
  const kids = [];
  function addRow(k, v) {
    const keyIn = h('input', { class: 'input sm mono', placeholder: 'key', value: k, 'aria-label': 'Key', disabled: ctx.disabled });
    const pointer = () => ctx.pointer + '/' + esc(keyIn.value);
    const inner = node(valueSchema, v, child(ctx, pointer(), true));
    const entry = { keyIn, inner };
    const del = ctx.disabled ? null : h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove entry', onclick: () => { row.remove(); kids.splice(kids.indexOf(entry), 1); ctx.emit(); } }, icon('x'));
    keyIn.addEventListener('input', ctx.emit);
    const row = h('div', { class: 'se-item' }, h('div', { style: { flex: '0 0 30%' } }, keyIn), h('div', { class: 'grow' }, inner.el), del);
    kids.push(entry);
    rows.appendChild(row);
  }
  for (const [k, v] of Object.entries(value || {})) addRow(k, v);
  const add = ctx.disabled ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => { addRow('', blank(valueSchema)); ctx.emit(); } }, icon('plus'), 'Add entry');
  return {
    el: h('div', { class: 'se-map stack-sm' }, h('span', { class: 'tiny muted' }, 'Additional entries'), rows, add),
    get() {
      const out = {};
      for (const k of kids) if (k.keyIn.value) out[k.keyIn.value] = k.inner.get();
      return out;
    },
  };
}

// ── Arrays ─────────────────────────────────────────────────────────

function arrayNode(s, value, ctx) {
  const itemSchema = normalise(s.items || {});
  const rows = h('div', { class: 'se-arr' });
  const count = h('span', { class: 'tiny muted' });
  let kids = [];
  function rebuild(list) {
    kids = [];
    clear(rows);
    list.forEach((v, i) => addRow(v, i));
    count.textContent = list.length === 1 ? '1 item' : list.length + ' items';
  }
  function addRow(v, i) {
    const pointer = ctx.pointer + '/' + i;
    const msg = h('div', { class: 'se-msg', hidden: true });
    const inner = node(itemSchema, v, child(ctx, pointer, true));
    const actions = ctx.disabled ? null : h('div', { class: 'se-item-actions' },
      h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Move item ' + i + ' up', title: 'Move up', disabled: i === 0, onclick: () => move(i, -1) }, icon('up')),
      h('button', { class: 'btn sm icon ghost', type: 'button', 'aria-label': 'Remove item ' + i, title: 'Remove', onclick: () => remove(i) }, icon('x')));
    const row = h('div', { class: 'se-item' }, h('span', { class: 'se-idx mono' }, String(i)), h('div', { class: 'grow', style: { minWidth: 0 } }, inner.el, msg), actions);
    ctx.fields.set(pointer, { wrap: row, msg });
    kids.push(inner);
    rows.appendChild(row);
  }
  function current() {
    return kids.map((k) => { const v = k.get(); return v === undefined ? null : v; });
  }
  function remove(i) {
    const list = current();
    list.splice(i, 1);
    rebuild(list);
    ctx.emit();
  }
  function move(i, d) {
    const list = current();
    const j = i + d;
    if (j < 0 || j >= list.length) return;
    [list[i], list[j]] = [list[j], list[i]];
    rebuild(list);
    ctx.emit();
  }
  rebuild(Array.isArray(value) ? value : []);
  const add = ctx.disabled ? null : h('button', { class: 'btn sm', type: 'button', onclick: () => { const list = current(); list.push(blank(itemSchema)); rebuild(list); ctx.emit(); } },
    icon('plus'), 'Add ' + (itemSchema.title || 'item'));
  return { el: h('div', { class: 'stack-sm' }, rows, h('div', { class: 'row' }, add, count)), get: current };
}

// ── oneOf / anyOf ──────────────────────────────────────────────────

function variantNode(s, value, ctx) {
  const base = Object.assign({}, s);
  delete base.oneOf;
  delete base.anyOf;
  const variants = (s.oneOf || s.anyOf).map((v) => normalise({ allOf: [base, v] }));
  const labels = variants.map((v, i) => v.title || (s.oneOf || s.anyOf)[i].title || typeLabel(v) || 'Option ' + (i + 1));
  let idx = 0;
  let best = -1;
  variants.forEach((v, i) => { const f = fit(v, value); if (f > best) { best = f; idx = i; } });
  const pick = h('select', { class: 'select sm', 'aria-label': 'Variant', disabled: ctx.disabled },
    ...labels.map((l, i) => h('option', { value: String(i) }, l)));
  pick.value = String(idx);
  const slot = h('div');
  let inner = null;
  function mount(v) {
    inner = node(variants[idx], v, Object.assign({}, ctx, { depth: ctx.depth + 1 }));
    replace(slot, inner.el);
  }
  pick.addEventListener('change', () => {
    idx = Number(pick.value);
    mount(blank(variants[idx]));
    ctx.emit();
  });
  mount(value === undefined ? blank(variants[idx]) : value);
  return {
    el: h('div', { class: 'stack-sm' }, h('div', { class: 'row nowrap' }, h('span', { class: 'tiny muted' }, s.oneOf ? 'One of' : 'Any of'), pick), slot),
    get: () => inner.get(),
  };
}

// ── Scalars ────────────────────────────────────────────────────────

function anyNode(s, value, ctx) {
  const ta = h('textarea', { class: 'textarea mono', rows: 3, spellcheck: 'false', placeholder: 'Any JSON value', disabled: ctx.disabled });
  ta.value = value === undefined ? '' : JSON.stringify(value, null, 2);
  ta.addEventListener('input', () => {
    let ok = true;
    if (ta.value.trim()) { try { JSON.parse(ta.value); } catch { ok = false; } }
    ta.classList.toggle('invalid', !ok);
    ctx.emit();
  });
  return {
    el: ta,
    get() {
      if (!ta.value.trim()) return ctx.required ? null : undefined;
      try { return JSON.parse(ta.value); } catch { return ta.value; }
    },
  };
}

function scalarNode(s, value, ctx, t) {
  const opt = !ctx.required;
  const dis = ctx.disabled;
  if (s.const !== undefined) {
    return { el: h('code', { class: 'se-const' }, JSON.stringify(s.const)), get: () => clone(s.const) };
  }
  if (Array.isArray(s.enum) && s.enum.length) {
    const sel = h('select', { class: 'select sm', disabled: dis });
    if (opt) sel.appendChild(h('option', { value: '' }, '(not set)'));
    s.enum.forEach((v, i) => sel.appendChild(h('option', { value: String(i) }, v === null ? 'null' : String(v))));
    const at = s.enum.findIndex((v) => JSON.stringify(v) === JSON.stringify(value));
    sel.value = at >= 0 ? String(at) : opt ? '' : '0';
    sel.addEventListener('change', ctx.emit);
    return { el: sel, get: () => (sel.value === '' ? undefined : clone(s.enum[Number(sel.value)])) };
  }
  if (t === 'boolean') {
    if (opt) {
      const sel = h('select', { class: 'select sm', disabled: dis }, h('option', { value: '' }, '(not set)'), h('option', { value: 'true' }, 'true'), h('option', { value: 'false' }, 'false'));
      sel.value = value === undefined ? '' : String(!!value);
      sel.addEventListener('change', ctx.emit);
      return { el: sel, get: () => (sel.value === '' ? undefined : sel.value === 'true') };
    }
    const cb = h('input', { type: 'checkbox', checked: !!value, disabled: dis });
    const text = h('span', { class: 'mono small' }, String(!!value));
    cb.addEventListener('change', () => { text.textContent = String(cb.checked); ctx.emit(); });
    return { el: h('label', { class: 'check' }, cb, text), get: () => cb.checked };
  }
  if (t === 'integer' || t === 'number') {
    const inp = h('input', { class: 'input sm mono', type: 'number', step: t === 'integer' ? '1' : 'any', min: s.minimum, max: s.maximum, disabled: dis, placeholder: opt ? '(not set)' : '' });
    inp.value = typeof value === 'number' ? String(value) : '';
    inp.addEventListener('input', ctx.emit);
    return {
      el: inp,
      get() {
        if (inp.value === '') return undefined;
        const n = Number(inp.value);
        return Number.isNaN(n) ? inp.value : n;
      },
    };
  }
  // Strings (by format).
  const str = typeof value === 'string' ? value : value === undefined || value === null ? '' : String(value);
  const read = (el) => () => (el.value === '' && opt ? undefined : el.value);
  if (s.format === 'date') {
    const inp = h('input', { class: 'input sm mono', type: 'date', disabled: dis, value: str });
    inp.addEventListener('input', ctx.emit);
    return { el: inp, get: read(inp) };
  }
  const long = (s.maxLength || 0) > 200 || (!s.format && /description|text|body|content|comment|notes?$/i.test(ctx.pointer.split('/').pop() || '') && !s.maxLength);
  const inp = long
    ? h('textarea', { class: 'textarea', rows: 2, disabled: dis, spellcheck: 'false' })
    : h('input', { class: 'input sm' + (s.format && s.format !== 'email' ? ' mono' : ''), type: 'text', disabled: dis, spellcheck: 'false', autocomplete: 'off', placeholder: opt ? '(not set)' : s.format === 'date-time' ? '2026-01-31T12:00:00Z' : '' });
  inp.value = str;
  inp.addEventListener('input', ctx.emit);
  let gen = null;
  if (!dis && (s.format === 'uuid' || s.format === 'date-time')) {
    gen = h('button', { class: 'btn sm', type: 'button', title: s.format === 'uuid' ? 'Generate a new UUID' : 'Use the current time',
      onclick: () => { inp.value = s.format === 'uuid' ? uuid() : nowIso(); ctx.emit(); } }, s.format === 'uuid' ? 'New' : 'Now');
  }
  return { el: gen ? h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, inp), gen) : inp, get: read(inp) };
}
