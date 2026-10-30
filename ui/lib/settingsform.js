// A form for one group of instance settings, generated from
// GET /_rustybin/settings (each setting's JSON Schema, value, source and
// lock state). Secrets render as a password field that says "set, ends in
// k3Qx" with Replace and Clear; a locked setting is read-only with the
// reason; the source (environment, saved in the instance, default) is shown
// under every field. Save sends one PATCH with what changed and reports
// the diff in a toast. Every control carries its setting key as aria-label,
// which is how the browser check (conformance/ui) verifies that each
// registered setting has a control.
import { h, field, input, select, textarea, notice, toast, icon, replace } from './dom.js';
import { SERVER, send } from './http.js';

const SOURCE_TEXT = {
  stored: 'Saved in this instance',
  env: 'From the environment',
  default: 'Default',
};

function sourceLine(setting) {
  const src = setting.source;
  const bits = [SOURCE_TEXT[src] || 'Not set'];
  if (src === 'env' && setting.env) bits.push('(' + setting.env + ')');
  else if (src !== 'env' && setting.env && setting.stored) bits.push('(overrides ' + setting.env + ')');
  if (setting.locked) bits.push('Locked by RUSTYBIN_SETTINGS_LOCK: the environment is authoritative.');
  return bits.join(' ');
}

function hintFor(setting) {
  return [setting.description, sourceLine(setting)].filter(Boolean).join(' ');
}

/** One control for a non-secret setting. Returns {el, read(), dirty()}. */
function valueControl(setting) {
  const schema = setting.schema || {};
  const type = Array.isArray(schema.type) ? schema.type[0] : schema.type;
  const key = setting.key;
  const initial = setting.value;
  const props = { 'aria-label': key };
  if (setting.locked) props.disabled = true;
  let ctl;
  let read;
  if (type === 'boolean') {
    ctl = select([['false', 'Off'], ['true', 'On']], initial ? 'true' : 'false', props);
    read = () => ctl.value === 'true';
  } else if (Array.isArray(schema.enum)) {
    ctl = select(schema.enum.map((v) => [v, v]), initial === null || initial === undefined ? setting.default : initial, props);
    read = () => ctl.value;
  } else if (type === 'integer' || type === 'number') {
    ctl = input(Object.assign({ type: 'number', class: 'input mono', value: initial === null || initial === undefined ? '' : String(initial), min: schema.minimum, max: schema.maximum, step: type === 'integer' ? '1' : 'any' }, props));
    read = () => (ctl.value === '' ? null : Number(ctl.value));
  } else if (type === 'array') {
    ctl = textarea(Object.assign({ rows: 3, placeholder: 'one per line, or comma separated' }, props));
    ctl.value = Array.isArray(initial) ? initial.join('\n') : '';
    read = () => ctl.value.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
  } else {
    ctl = input(Object.assign({ value: initial === null || initial === undefined ? '' : String(initial), type: schema.format === 'uri' ? 'url' : 'text', placeholder: schema.format === 'uri' ? 'https://portal.example.com' : '', maxlength: schema.maxLength }, props));
    read = () => (ctl.value.trim() === '' ? null : ctl.value.trim());
  }
  const same = (a, b) => JSON.stringify(a === undefined ? null : a) === JSON.stringify(b === undefined ? null : b);
  const norm = (v) => (Array.isArray(v) && v.length === 0 ? null : v);
  return {
    el: field(setting.title, ctl, hintFor(setting)),
    read,
    dirty: () => !same(norm(read()), norm(initial)),
    focus: () => ctl.focus(),
  };
}

/** A secret: status, a Replace button that reveals a password field, Clear. */
function secretControl(setting) {
  const st = setting.value || {};
  const key = setting.key;
  let mode = 'keep'; // keep | replace | clear
  const pw = input({ type: 'password', 'aria-label': key, placeholder: 'new value', autocomplete: 'new-password', hidden: true });
  if (setting.locked) pw.disabled = true;
  const status = h('span', { class: 'secret-status' });
  const replaceBtn = h('button', { class: 'btn sm', type: 'button', 'aria-label': key + ' replace' }, setting.locked ? 'Locked' : (st.set ? 'Replace' : 'Set'));
  const clearBtn = h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': key + ' clear' }, 'Clear');
  const cancelBtn = h('button', { class: 'btn sm ghost', type: 'button', 'aria-label': key + ' cancel', hidden: true }, 'Keep current');
  if (setting.locked) { replaceBtn.disabled = true; clearBtn.disabled = true; }
  function renderStatus() {
    if (mode === 'clear') {
      replace(status, h('span', { class: 'badge warn' }, 'will be cleared on save'));
      return;
    }
    if (mode === 'replace') {
      replace(status, h('span', { class: 'badge info' }, 'new value on save'));
      return;
    }
    if (st.unreadable) {
      replace(status, h('span', { class: 'badge err' }, icon('alert'), 'stored but unreadable: the secrets key changed, set it again'));
    } else if (st.set) {
      replace(status, h('span', { class: 'badge ok' }, icon('lock'), 'Set' + (st.hint ? ', ends in ' + st.hint : '')),
        h('span', { class: 'muted small' }, ' ' + (SOURCE_TEXT[st.source] || '')));
    } else {
      replace(status, h('span', { class: 'badge' }, 'Not set'));
    }
  }
  replaceBtn.addEventListener('click', () => { mode = 'replace'; pw.hidden = false; cancelBtn.hidden = false; pw.focus(); renderStatus(); });
  clearBtn.addEventListener('click', () => { mode = 'clear'; pw.hidden = true; pw.value = ''; cancelBtn.hidden = false; renderStatus(); });
  cancelBtn.addEventListener('click', () => { mode = 'keep'; pw.hidden = true; pw.value = ''; cancelBtn.hidden = true; renderStatus(); });
  renderStatus();
  const row = h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } }, status, h('span', { class: 'grow' }), replaceBtn, st.set && !setting.locked ? clearBtn : null, cancelBtn);
  const el = h('div', { class: 'field' }, h('span', null, setting.title), row, pw, h('span', { class: 'hint' }, hintFor(setting)));
  return {
    el,
    // null clears, undefined keeps, a string replaces.
    read: () => (mode === 'clear' ? null : mode === 'replace' ? pw.value : undefined),
    dirty: () => mode === 'clear' || (mode === 'replace' && pw.value !== ''),
    focus: () => pw.focus(),
  };
}

/**
 * The form of one settings group. `doc` is the GET /_rustybin/settings body;
 * `group` the group id. opts.saveLabel names the Save button (unique per
 * page), opts.onSaved(doc) runs after a successful save with the new body.
 * Returns {el, refresh(doc)}.
 */
export function settingsForm(doc, group, opts = {}) {
  const root = h('div', { class: 'stack' });
  let controls = [];
  const err = h('div');

  function build(d) {
    controls = [];
    const items = (d.settings || []).filter((s) => s.group === group);
    const body = h('div', { class: 'stack' });
    if (!items.length) body.appendChild(notice('', 'No settings are registered in this group.'));
    for (const s of items) {
      const c = s.secret ? secretControl(s) : valueControl(s);
      c.setting = s;
      controls.push(c);
      body.appendChild(c.el);
    }
    const allLocked = items.length > 0 && items.every((s) => s.locked);
    const saveBtn = h('button', { class: 'btn primary', type: 'submit', disabled: allLocked }, opts.saveLabel || 'Save');
    const foot = h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '8px' } },
      d.persisted ? h('span', { class: 'muted small inline-icon' }, icon('check'), ' Changes are saved to the state directory.')
        : h('span', { class: 'muted small inline-icon' }, icon('info'), ' No RUSTYBIN_STATE_DIR: changes last until the instance restarts.'),
      h('span', { class: 'grow' }), saveBtn);
    const form = h('form', { class: 'stack', 'aria-label': 'Settings: ' + group }, body, err, foot);
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      await save();
    });
    replace(root, form);
  }

  async function save() {
    const values = {};
    const secrets = {};
    const changed = [];
    for (const c of controls) {
      if (!c.dirty()) continue;
      const v = c.read();
      if (c.setting.secret) secrets[c.setting.key] = v; else values[c.setting.key] = v;
      changed.push(c.setting.key);
    }
    if (!changed.length) { toast('Nothing to save'); return; }
    const r = await send({ method: 'PATCH', url: SERVER + '/_rustybin/settings', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ values, secrets }), admin: true, session: false });
    if (r.error || !r.ok) {
      const j = r.json || {};
      const detail = (j.problems || j.locked || []).join('; ');
      replace(err, notice('err', r.error || j.error || ('Save failed (' + r.status + ')'), detail ? ' ' + detail : ''));
      return;
    }
    replace(err);
    toast('Saved ' + changed.join(', '));
    build(r.json);
    if (opts.onSaved) opts.onSaved(r.json);
  }

  build(doc);
  return { el: root, refresh: build };
}
