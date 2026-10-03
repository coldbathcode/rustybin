// Forms generated from a JSON Schema object (MCP tool inputSchema,
// elicitation requestedSchema, prompt arguments).
import { h, field, input, textarea, select } from './dom.js';

/**
 * Build a form for an object schema. Returns {el, value()} where value()
 * returns the arguments object or throws an Error with a readable message.
 */
export function schemaForm(schema, initial) {
  const props = (schema && schema.properties) || {};
  const required = new Set((schema && schema.required) || []);
  const controls = [];
  const el = h('div', { class: 'stack-sm' });
  const names = Object.keys(props);
  if (!names.length) el.appendChild(h('p', { class: 'small muted' }, 'No arguments.'));
  for (const name of names) {
    const p = props[name] || {};
    const type = Array.isArray(p.type) ? p.type.find((t) => t !== 'null') : p.type;
    const init = initial && initial[name] !== undefined ? initial[name] : p.default;
    const label = (p.title || name) + (required.has(name) ? ' *' : '');
    const hint = [p.description, type ? `(${type}${p.minimum !== undefined ? `, min ${p.minimum}` : ''}${p.maximum !== undefined ? `, max ${p.maximum}` : ''})` : null].filter(Boolean).join(' ');
    let ctl, read;
    if (Array.isArray(p.enum)) {
      ctl = select([['', required.has(name) ? 'Choose...' : '(not set)'], ...p.enum.map((v) => [String(v), String(v)])], init !== undefined ? String(init) : '');
      read = () => (ctl.value === '' ? undefined : (type === 'integer' || type === 'number' ? Number(ctl.value) : ctl.value));
    } else if (type === 'boolean') {
      ctl = h('select', { class: 'select' }, h('option', { value: '' }, '(not set)'), h('option', { value: 'true' }, 'true'), h('option', { value: 'false' }, 'false'));
      if (init !== undefined) ctl.value = String(!!init);
      read = () => (ctl.value === '' ? undefined : ctl.value === 'true');
    } else if (type === 'integer' || type === 'number') {
      ctl = input({ type: 'number', class: 'input mono', value: init !== undefined ? String(init) : '', step: type === 'integer' ? '1' : 'any' });
      read = () => {
        if (ctl.value === '') return undefined;
        const n = Number(ctl.value);
        if (Number.isNaN(n)) throw new Error(`${name} must be a number`);
        if (type === 'integer' && !Number.isInteger(n)) throw new Error(`${name} must be an integer`);
        return n;
      };
    } else if (type === 'object' || type === 'array' || !type) {
      ctl = textarea({ rows: 3, placeholder: type === 'array' ? '["a", "b"]' : '{"key": "value"}' });
      ctl.value = init !== undefined ? JSON.stringify(init, null, 2) : '';
      read = () => {
        if (!ctl.value.trim()) return undefined;
        try { return JSON.parse(ctl.value); } catch { if (!type) return ctl.value; throw new Error(`${name} must be valid JSON`); }
      };
    } else {
      const long = (p.maxLength || 0) > 200 || /text|code|body|content|prompt/i.test(name);
      ctl = long ? textarea({ rows: 3, class: 'textarea' }) : input({ value: init !== undefined ? String(init) : '' });
      if (long && init !== undefined) ctl.value = String(init);
      read = () => (ctl.value === '' ? undefined : ctl.value);
    }
    ctl.setAttribute('aria-label', name);
    controls.push({ name, read, required: required.has(name) });
    el.appendChild(field(label, ctl, hint || null));
  }
  return {
    el,
    value() {
      const out = {};
      for (const c of controls) {
        const v = c.read();
        if (v === undefined) {
          if (c.required) throw new Error(`${c.name} is required`);
          continue;
        }
        out[c.name] = v;
      }
      return out;
    },
  };
}

/** Prompt arguments ([{name, description, required}]) as a schema. */
export function promptArgsSchema(args) {
  const properties = {};
  const required = [];
  for (const a of args || []) {
    properties[a.name] = { type: 'string', description: a.description };
    if (a.required) required.push(a.name);
  }
  return { type: 'object', properties, required };
}
