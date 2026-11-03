// The compare view of a stale edit: an edit refused with 409
// rustybin_stale_revision (someone changed the mock since this page read it)
// opens a dialog that shows, per section (the spec, the responses, security,
// the MCP server, each operation's settings and examples), what the other
// change did beside the edit that was refused, both from the revision the
// edit was made from (GET /_rustybin/mocks/{name}/revisions/{n}). "Keep mine"
// sends the refused edit again naming the current revision (the sections it
// sets replace theirs, the rest of their change stays); "Take theirs" reloads
// the page; Close (or Escape) keeps the form as it is, nothing saved.
// Everything shown is built with textContent (the spec is user content).
import { h, icon, notice, spinner, replace, badge } from './dom.js';
import { diffView, normaliseBody } from './format.js';

const enc = encodeURIComponent;

/** Sorted keys, without null, empty lists and empty objects (the server leaves those out). */
function clean(v) {
  if (Array.isArray(v)) return v.map(clean);
  if (v && typeof v === 'object') {
    const out = {};
    for (const k of Object.keys(v).sort()) {
      const c = clean(v[k]);
      if (c === null || c === undefined) continue;
      if (Array.isArray(c) && !c.length) continue;
      if (typeof c === 'object' && !Array.isArray(c) && !Object.keys(c).length) continue;
      out[k] = c;
    }
    return out;
  }
  return v;
}

function stable(v) {
  if (v === undefined || v === null) return '(none)';
  return JSON.stringify(clean(v), null, 2);
}

function specText(v) {
  if (typeof v === 'string') return normaliseBody(v);
  return JSON.stringify(v, null, 2);
}

/** The sections of a revision (the answer of GET .../revisions/{n}), as text. */
export function sectionsOf(rev) {
  const s = rev.settings || {};
  const out = new Map();
  out.set('spec', specText((rev.spec && rev.spec.text) || ''));
  out.set('server prefix', stable(!!s.strip_server_prefix));
  out.set('responses', stable(s.responses));
  out.set('security', stable(s.security));
  out.set('mcp', stable(s.mcp || { enabled: true }));
  for (const [id, o] of Object.entries(s.operations || {})) out.set('operation ' + id, stable(o));
  return out;
}

/**
 * The sections after the refused edit, made on `sections`. `edit` is
 * {op, body} for PUT .../operations/{op} (body null clears it), else {body}
 * for a PATCH (each section it names replaces the section as a whole).
 */
export function applyEdit(sections, edit) {
  const m = new Map(sections);
  const b = edit.body;
  if (edit.op) {
    const key = 'operation ' + edit.op;
    if (b === null || b === undefined || !Object.keys(clean(b)).length) m.delete(key);
    else m.set(key, stable(b));
    return m;
  }
  if (!b || typeof b !== 'object') return m;
  if ('spec' in b) m.set('spec', specText(b.spec));
  if ('strip_server_prefix' in b) m.set('server prefix', stable(!!b.strip_server_prefix));
  for (const k of ['responses', 'security', 'mcp']) if (k in b) m.set(k, stable(b[k]));
  if ('operations' in b) {
    for (const k of [...m.keys()]) if (k.startsWith('operation ')) m.delete(k);
    for (const [id, o] of Object.entries(b.operations || {})) m.set('operation ' + id, stable(o));
  }
  return m;
}

/** The section names changed on either side, in a stable order. */
export function changedSections(base, theirs, mine) {
  const keys = [...new Set([...base.keys(), ...theirs.keys(), ...mine.keys()])];
  const rank = (k) => ['spec', 'server prefix', 'responses', 'security', 'mcp'].indexOf(k);
  keys.sort((a, b) => {
    const ra = rank(a), rb = rank(b);
    if (ra >= 0 || rb >= 0) return (ra < 0 ? 99 : ra) - (rb < 0 ? 99 : rb);
    return a < b ? -1 : a > b ? 1 : 0;
  });
  const get = (m, k) => (m.has(k) ? m.get(k) : '(none)');
  return keys
    .map((k) => ({ key: k, base: get(base, k), theirs: get(theirs, k), mine: get(mine, k) }))
    .filter((s) => s.base !== s.theirs || s.base !== s.mine);
}

const TITLES = { spec: 'Spec', 'server prefix': 'Server prefix', responses: 'Responses', security: 'Security', mcp: 'MCP server' };

function title(key) {
  if (TITLES[key]) return TITLES[key];
  return 'Operation ' + key.slice('operation '.length) + ' (settings, rules, examples)';
}

/** The edit of a request, or null when the request is not an edit the view can show. */
export function editOf(method, path, body) {
  const segs = (path || '').split('?')[0].split('/');
  let name;
  try { name = decodeURIComponent(segs[1] || ''); } catch { return null; }
  if (!name) return null;
  if (method === 'PATCH' && segs.length === 2 && body && typeof body === 'object') return { name, body };
  if (method === 'PUT' && segs.length === 4 && segs[2] === 'operations') {
    let op;
    try { op = decodeURIComponent(segs[3]); } catch { return null; }
    return { name, op, body: body === undefined ? null : body };
  }
  return null;
}

function modal(id) {
  const dlg = h('dialog', { class: 'wizard wide', id, 'aria-labelledby': id + '-title' });
  const head = h('h2', { id: id + '-title', class: 'grow' });
  const body = h('div', { class: 'wizard-body stack' });
  const foot = h('div', { class: 'wizard-foot row' });
  dlg.append(h('div', { class: 'wizard-head row nowrap' }, head,
    h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, icon('x'))), body, foot);
  document.body.appendChild(dlg);
  dlg.showModal();
  return { dlg, head, body, foot };
}

function column(label, changed, from, to, unchanged) {
  return h('div', { class: 'stack-sm conflict-col' },
    h('span', { class: 'label' }, label),
    changed ? diffView(from, to) : h('p', { class: 'muted small', style: { margin: 0 } }, unchanged));
}

/**
 * Show the compare dialog of a refused edit. `conflict` is the 409's body,
 * `get(path)` reads the management API, `resend(revision)` sends the edit
 * again naming `revision`. Resolves with the answer of the edit sent again
 * ("Keep mine"), or null when the dialog was closed (the form stays).
 */
export function compareStale({ edit, conflict, get, resend }) {
  return new Promise((resolve) => {
    const { dlg, head, body, foot } = modal('conflict-dialog');
    const { name } = edit;
    const expected = conflict.expected_revision;
    const current = conflict.current_revision;
    head.textContent = `${name} changed while you were editing`;
    let settled = false;
    const done = (v) => { if (!settled) { settled = true; resolve(v); } };
    dlg.addEventListener('close', () => { dlg.remove(); done(null); });

    const out = h('div', { class: 'stack', id: 'conflict-sections' }, spinner('Reading both revisions'));
    const keepMsg = h('div');
    const keepBtn = h('button', { class: 'btn primary', type: 'button', id: 'conflict-keep', disabled: true }, icon('check'), 'Keep mine');
    const theirsBtn = h('button', { class: 'btn', type: 'button', id: 'conflict-theirs' }, icon('refresh'), 'Take theirs');
    theirsBtn.addEventListener('click', () => { dlg.close(); window.location.reload(); });
    keepBtn.addEventListener('click', async () => {
      keepBtn.disabled = true;
      theirsBtn.disabled = true;
      replace(keepMsg, spinner(`Saving your edit on revision ${current}`));
      const r = await resend(current);
      if (r && r.ok) {
        settled = true;
        resolve(r);
        dlg.close();
        return;
      }
      keepBtn.disabled = false;
      theirsBtn.disabled = false;
      const j = (r && r.json) || {};
      if (r && r.status === 409 && j.error === 'rustybin_stale_revision') {
        // Changed again meanwhile: this answer opened a new compare view.
        settled = true;
        resolve(r);
        dlg.close();
        return;
      }
      replace(keepMsg, notice('err', 'Your edit was not saved: ', (j.message || j.error || (r && r.error) || `the answer was ${r ? r.status : 'empty'}`)));
    });

    body.append(
      notice('warn', h('strong', null, `Revision ${current} was saved after you loaded revision ${expected}. `),
        'Nothing of yours was saved. Below, per section, what the other change did beside your edit, both from revision ', String(expected), '.'),
      out, keepMsg);
    foot.append(h('span', { class: 'muted small grow' }, `Keep mine saves your edit on revision ${current}: the sections you edited replace theirs, the rest of their change stays. Take theirs reloads.`),
      h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Close'), theirsBtn, keepBtn);

    (async () => {
      const path = (n) => `/${enc(name)}/revisions/${enc(String(n))}`;
      const [baseR, curR] = await Promise.all([get(path(expected)), get(path(current))]);
      if (!dlg.open) return;
      keepBtn.disabled = false;
      if (!curR.ok || !curR.json) {
        replace(out, notice('err', `The current revision could not be read (${curR.status || curR.error}). Keep mine still saves your edit on it.`));
        return;
      }
      const theirs = sectionsOf(curR.json);
      if (!baseR.ok || !baseR.json) {
        // The revision the edit was made from is gone: the current draft beside the edit.
        const mine = applyEdit(theirs, edit);
        const rows = changedSections(theirs, theirs, mine);
        replace(out,
          notice('', `Revision ${expected} is no longer kept, so each section shows the current draft (revision ${current}) changed into your edit.`),
          ...rows.map((s) => h('section', { class: 'stack-sm conflict-section', 'aria-label': title(s.key), dataset: { section: s.key } },
            h('div', { class: 'row' }, h('strong', null, title(s.key)), badge('your edit', 'outline')),
            column(`Revision ${current}, then your edit`, true, s.theirs, s.mine, ''))),
          rows.length ? null : h('p', { class: 'muted' }, 'Your edit changes nothing in the current draft.'));
        return;
      }
      const base = sectionsOf(baseR.json);
      const mine = applyEdit(base, edit);
      const rows = changedSections(base, theirs, mine);
      replace(out, ...rows.map((s) => {
        const them = s.base !== s.theirs;
        const you = s.base !== s.mine;
        const both = them && you;
        const tag = both ? (s.theirs === s.mine ? badge('same change on both sides', 'ok') : badge('changed on both sides: yours replaces theirs', 'warn'))
          : them ? badge('their change stays', 'outline') : badge('your edit', 'outline');
        return h('section', { class: 'stack-sm conflict-section', 'aria-label': title(s.key), dataset: { section: s.key } },
          h('div', { class: 'row' }, h('strong', null, title(s.key)), tag),
          h('div', { class: 'grid grid-2' },
            column(`Their change (revision ${expected} to ${current})`, them, s.base, s.theirs, 'Not changed by them.'),
            column(`Your edit (from revision ${expected})`, you, s.base, s.mine, 'Not changed by you.')));
      }), rows.length ? null : h('p', { class: 'muted' }, 'No section differs: the other change and your edit leave the definition as it was.'));
    })();
  });
}
