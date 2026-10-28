// A form for harness scenarios, generated from the scenario JSON Schema
// (GET /_rustybin/harness/schema): nested objects, arrays of objects with
// add and remove, oneOf branches picked by their `type` (patterns, with a
// lazily added `then` stage), `$ref`s, durations, header maps and status
// lists. Every control carries its schema path as aria-label and every add
// button `<path> add`, which is how the browser check (conformance/ui)
// verifies that each non-shorthand schema property has a control, so a
// parameter added to the model shows up here without further work.
import { h, field, input, select, textarea } from './dom.js';

function resolve(root, node) {
  let guard = 0;
  while (node && node.$ref && guard++ < 10) {
    const path = node.$ref.replace(/^#\//, '').split('/');
    let cur = root;
    for (const seg of path) cur = cur ? cur[seg] : undefined;
    node = Object.assign({}, cur, { description: node.description || (cur && cur.description) });
  }
  return node || {};
}

function typeOf(node) {
  return Array.isArray(node.type) ? node.type.filter((x) => x !== 'null') : node.type ? [node.type] : [];
}

function hint(node) {
  const parts = [node.description];
  if (node.minimum !== undefined || node.maximum !== undefined) {
    parts.push(`(${node.minimum !== undefined ? 'min ' + node.minimum : ''}${node.minimum !== undefined && node.maximum !== undefined ? ', ' : ''}${node.maximum !== undefined ? 'max ' + node.maximum : ''})`);
  }
  if (node.default !== undefined) parts.push(`Default ${JSON.stringify(node.default)}.`);
  return parts.filter(Boolean).join(' ');
}

/** Is this a oneOf whose branches are told apart by a `type` constant? */
function isDiscriminated(root, node) {
  return Array.isArray(node.oneOf) && node.oneOf.every((b) => {
    const r = resolve(root, b);
    return r.properties && r.properties.type;
  });
}

/** The object branch of a oneOf (targets are a string or an object). */
function objectBranch(root, node) {
  if (node.properties) return node;
  if (node.oneOf) {
    const found = node.oneOf.map((b) => resolve(root, b)).find((b) => b.properties);
    if (found) return found;
  }
  return node;
}

function addButton(path, label, onclick) {
  return h('button', { class: 'btn sm', type: 'button', 'aria-label': path + ' add', onclick }, label);
}

/**
 * Build a control for `node` with the current `value`. Returns {el, read()}:
 * read() gives the value or undefined when unset, throws Error on bad input.
 */
function control(root, node, label, value, path) {
  node = resolve(root, node);
  const types = typeOf(node);
  if (isDiscriminated(root, node)) return discriminated(root, node, label, value, path);
  if (node.oneOf && !node.properties && !types.includes('string')) {
    return control(root, objectBranch(root, node), label, value, path);
  }
  if (Array.isArray(node.enum) || node.const !== undefined) {
    const values = node.const !== undefined ? [node.const] : node.enum;
    const ctl = select([['', '(not set)'], ...values.map((v) => [String(v), String(v)])], value !== undefined ? String(value) : (node.default !== undefined ? String(node.default) : ''));
    ctl.setAttribute('aria-label', path);
    return { el: field(label, ctl, hint(node)), read: () => (ctl.value === '' ? undefined : ctl.value) };
  }
  if (types.length === 1 && types[0] === 'boolean') {
    const ctl = select([['', '(not set)'], ['true', 'true'], ['false', 'false']], value === undefined ? '' : String(!!value));
    ctl.setAttribute('aria-label', path);
    return { el: field(label, ctl, hint(node)), read: () => (ctl.value === '' ? undefined : ctl.value === 'true') };
  }
  if (node.format === 'duration') {
    const ctl = input({ value: value === undefined ? '' : String(value), placeholder: '30s, 2m, 500ms', class: 'input mono', 'aria-label': path });
    return { el: field(label, ctl, hint(node)), read: () => (ctl.value.trim() === '' ? undefined : (/^\d+(\.\d+)?$/.test(ctl.value.trim()) ? Number(ctl.value) : ctl.value.trim())) };
  }
  if (types.length === 1 && (types[0] === 'number' || types[0] === 'integer')) {
    const ctl = input({ type: 'number', class: 'input mono', value: value === undefined ? '' : String(value), step: types[0] === 'integer' ? '1' : 'any', min: node.minimum, max: node.maximum, 'aria-label': path });
    return {
      el: field(label, ctl, hint(node)),
      read: () => {
        if (ctl.value === '') return undefined;
        const n = Number(ctl.value);
        if (Number.isNaN(n)) throw new Error(`${path} must be a number`);
        if (types[0] === 'integer' && !Number.isInteger(n)) throw new Error(`${path} must be an integer`);
        return n;
      },
    };
  }
  // Free-form values (a request body): text, or JSON when it parses.
  if (types.length > 1 || (types.length === 0 && !node.properties && !node.oneOf)) {
    const ctl = textarea({ rows: 3, placeholder: 'text, or JSON', 'aria-label': path });
    ctl.value = value === undefined ? '' : typeof value === 'string' ? value : JSON.stringify(value, null, 2);
    return {
      el: field(label, ctl, hint(node)),
      read: () => {
        const text = ctl.value;
        if (!text.trim()) return undefined;
        const t = text.trim();
        if ((t.startsWith('{') && t.endsWith('}')) || (t.startsWith('[') && t.endsWith(']'))) {
          try { return JSON.parse(t); } catch { return text; }
        }
        return text;
      },
    };
  }
  if (types[0] === 'array') {
    const items = resolve(root, node.items || {});
    const itemTypes = typeOf(items);
    if (!items.properties && !items.oneOf && (itemTypes.includes('integer') || itemTypes.includes('number') || itemTypes.includes('string'))) {
      const ctl = input({ value: Array.isArray(value) ? value.join(', ') : '', class: 'input mono', placeholder: itemTypes.includes('string') ? 'a, b' : '200, 404', 'aria-label': path });
      return {
        el: field(label, ctl, hint(node)),
        read: () => {
          const text = ctl.value.trim();
          if (!text) return undefined;
          return text.split(',').map((x) => x.trim()).filter(Boolean).map((x) => {
            if (itemTypes.includes('string')) return x;
            const n = Number(x);
            if (Number.isNaN(n)) throw new Error(`${path}: ${x} is not a number`);
            return n;
          });
        },
      };
    }
    return objectList(root, node, label, Array.isArray(value) ? value : [], path);
  }
  if (types[0] === 'object' && node.additionalProperties && typeof node.additionalProperties === 'object' && !node.properties) {
    return mapEditor(node, label, value || {}, path);
  }
  if (node.properties) {
    return objectForm(root, node, label, value || {}, path, true);
  }
  const long = (node.maxLength || 0) > 300;
  const ctl = long ? textarea({ rows: 2, 'aria-label': path }) : input({ value: value === undefined ? '' : String(value), 'aria-label': path, placeholder: node.format === 'uri' ? 'https://gateway.example.com' : '' });
  if (long && value !== undefined) ctl.value = String(value);
  return { el: field(label, ctl, hint(node)), read: () => (ctl.value === '' ? undefined : ctl.value) };
}

/** A oneOf picked by the `type` property (patterns). `then` is added lazily. */
function discriminated(root, node, label, value, path) {
  const branches = node.oneOf.map((b) => resolve(root, b));
  const names = branches.map((b) => b.properties.type.const || b.title);
  const current = value && value.type && names.includes(value.type) ? value.type : names[0];
  const sel = select(names.map((n) => [n, n]), current, { 'aria-label': path + '.type' });
  const body = h('div', { class: 'stack-sm' });
  let inner = null;
  function render(kind, val) {
    const branch = branches[names.indexOf(kind)];
    const props = Object.assign({}, branch.properties);
    delete props.type;
    inner = objectForm(root, Object.assign({}, branch, { properties: props }), null, val || {}, path, false);
    body.replaceChildren(inner.el);
  }
  render(current, value);
  sel.addEventListener('change', () => render(sel.value, {}));
  const wrap = h('div', { class: 'stack-sm se-group' }, field(label ? `${label}: type` : 'type', sel, node.description || null), body);
  return {
    el: wrap,
    read: () => {
      const v = inner.read() || {};
      v.type = sel.value;
      return v;
    },
  };
}

/** An optional nested object (a `then` stage): a button until it is added. */
function lazy(root, node, label, value, path) {
  const slot = h('div', { class: 'stack-sm' });
  let inner = null;
  function show(val) {
    inner = control(root, node, label, val, path);
    const remove = h('button', { class: 'btn sm ghost', type: 'button', onclick: () => { inner = null; slot.replaceChildren(add); } }, 'Remove ' + label);
    slot.replaceChildren(h('div', { class: 'stack-sm' }, inner.el, remove));
  }
  const add = addButton(path, 'Add ' + label, () => show({}));
  if (value !== undefined) show(value);
  else slot.appendChild(add);
  return { el: slot, read: () => (inner ? inner.read() : undefined) };
}

/** key = value rows for a map (headers, status weights); values follow the
 *  additionalProperties type. */
function mapEditor(node, label, value, path) {
  const valueType = typeOf(node.additionalProperties || {})[0];
  const numeric = valueType === 'integer' || valueType === 'number';
  const rows = h('div', { class: 'stack-sm' });
  function row(k, v) {
    const key = input({ value: k || '', placeholder: numeric ? '200' : 'Header-Name', class: 'input mono', 'aria-label': path + ' key' });
    const val = input({ value: v === undefined || v === null ? '' : String(v), placeholder: numeric ? '1' : 'value', class: 'input mono', 'aria-label': path + ' value', type: numeric ? 'number' : 'text' });
    const del = h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': 'Remove', onclick: () => r.remove() }, 'x');
    const r = h('div', { class: 'row nowrap' }, key, val, del);
    r.dataset.row = '1';
    r.read = () => [key.value.trim(), val.value];
    rows.appendChild(r);
  }
  for (const [k, v] of Object.entries(value)) row(k, v);
  const el = h('div', { class: 'field' }, h('span', null, label), rows, addButton(path, 'Add ' + label, () => row('', '')), node.description ? h('span', { class: 'hint' }, node.description) : null);
  return {
    el,
    read: () => {
      const out = {};
      for (const r of rows.querySelectorAll('[data-row]')) {
        const [k, v] = r.read();
        if (!k) continue;
        if (numeric) {
          const n = Number(v);
          if (v === '' || Number.isNaN(n)) throw new Error(`${path}.${k} must be a number`);
          out[k] = n;
        } else {
          out[k] = v;
        }
      }
      return Object.keys(out).length ? out : undefined;
    },
  };
}

/** A list of objects (targets, layers, requests) with add and remove. */
function objectList(root, node, label, values, path) {
  const itemSchema = objectBranch(root, resolve(root, node.items || {}));
  const list = h('div', { class: 'stack-sm' });
  let n = 0;
  function item(val) {
    const i = n++;
    const form = objectForm(root, itemSchema, null, val || {}, `${path}[${i}]`, false);
    const del = h('button', { class: 'btn sm ghost', type: 'button', onclick: () => wrap.remove() }, 'Remove');
    const wrap = h('details', { class: 'disclosure se-item', open: true },
      h('summary', null, `${label} ${i + 1}`, ' ', del), h('div', { class: 'stack-sm', style: { paddingTop: '6px' } }, form.el));
    wrap.read = form.read;
    wrap.dataset.item = '1';
    list.appendChild(wrap);
  }
  for (const v of values) item(v);
  const el = h('div', { class: 'field' }, h('span', null, label), node.description ? h('span', { class: 'hint' }, node.description) : null, list, addButton(path, 'Add ' + label.replace(/s$/, ''), () => item({})));
  return {
    el,
    read: () => {
      const out = [];
      for (const w of list.querySelectorAll(':scope > [data-item]')) {
        const v = w.read();
        if (v !== undefined) out.push(v);
      }
      return out.length ? out : undefined;
    },
  };
}

/** The properties of an object schema as fields (shorthands skipped). */
function objectForm(root, node, label, value, path, collapsible) {
  const props = node.properties || {};
  const required = new Set(node.required || []);
  const controls = [];
  const body = h('div', { class: 'stack-sm' });
  for (const [key, sub] of Object.entries(props)) {
    const resolved = resolve(root, sub);
    if (resolved['x-shorthand'] || sub['x-shorthand']) continue;
    const here = path ? `${path}.${key}` : key;
    const text = key + (required.has(key) ? ' *' : '');
    const c = key === 'then' ? lazy(root, sub, 'then', value[key], here) : control(root, sub, text, value[key], here);
    controls.push({ key, c, required: required.has(key) });
    body.appendChild(c.el);
  }
  const el = collapsible && label
    ? h('details', { class: 'disclosure se-group', open: Object.keys(value).length > 0 }, h('summary', null, label), h('div', { class: 'stack-sm', style: { paddingTop: '6px' } }, body))
    : body;
  return {
    el,
    read: () => {
      const out = {};
      const missing = [];
      for (const { key, c, required: req } of controls) {
        const v = c.read();
        if (v === undefined) {
          if (req) missing.push(key);
          continue;
        }
        out[key] = v;
      }
      // An optional nested object left empty (a layer's expect) is absent;
      // once one of its fields is set, its required fields are too.
      if (collapsible && !Object.keys(out).length) return undefined;
      if (missing.length) throw new Error(`${path ? path + '.' : ''}${missing[0]} is required`);
      return Object.keys(out).length ? out : undefined;
    },
  };
}

/**
 * The scenario builder. Returns {el, value(), set(scenario)}: value() is the
 * scenario object (throws an Error with a readable message), set() rebuilds
 * the form for another scenario (a built-in one, or a validated YAML text).
 */
export function scenarioForm(schema, initial) {
  const el = h('div', { class: 'stack', id: 'scenario-form' });
  let form = null;
  function set(value) {
    form = objectForm(schema, schema, null, value || {}, '', false);
    el.replaceChildren(form.el);
  }
  set(initial);
  return { el, value: () => form.read() || {}, set };
}

/** A small YAML rendering of a scenario (the server parses YAML back). */
export function toYaml(value, indent = 0) {
  const pad = '  '.repeat(indent);
  const scalar = (v) => {
    if (v === null) return 'null';
    if (typeof v === 'string') return /^[A-Za-z_./][^#:]*$/.test(v) && !/^(true|false|null|yes|no|on|off)$/i.test(v) ? v : JSON.stringify(v);
    return String(v);
  };
  if (Array.isArray(value)) {
    if (!value.length) return pad + '[]';
    return value.map((v) => (typeof v === 'object' && v !== null ? pad + '-\n' + toYaml(v, indent + 1) : pad + '- ' + scalar(v))).join('\n');
  }
  if (typeof value === 'object' && value !== null) {
    const keys = Object.keys(value);
    if (!keys.length) return pad + '{}';
    return keys.map((k) => {
      const v = value[k];
      if (v !== null && typeof v === 'object') {
        if (Array.isArray(v) && !v.length) return `${pad}${k}: []`;
        if (!Array.isArray(v) && !Object.keys(v).length) return `${pad}${k}: {}`;
        return `${pad}${k}:\n${toYaml(v, indent + 1)}`;
      }
      return `${pad}${k}: ${scalar(v)}`;
    }).join('\n');
  }
  return pad + scalar(value);
}
