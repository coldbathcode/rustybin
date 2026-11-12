// Management, Packs: a customer's demo as one document (mocks, their portal
// items, saved tests, schedules, storyboards) applied into a project of its
// own and torn down as a unit. POST /_rustybin/packs/apply (dry_run
// previews), GET /_rustybin/packs, DELETE /_rustybin/packs/{name}, GET
// /_rustybin/packs/{name}/export. The same on every surface: rustybin pack,
// the *_pack(s) tools.
import { h, replace, card, notice, toast, icon, badge, spinner, fmtTime, checkbox } from '../../lib/dom.js';
import { SERVER, getJson, send } from '../../lib/http.js';
import * as perms from '../../lib/perms.js';

const BASE = '/_rustybin/packs';
const KINDS = [['mocks', 'mocks'], ['portal_items', 'portal items'], ['tests', 'saved tests'], ['test_schedules', 'schedules'], ['storyboards', 'storyboards'], ['checkpoints', 'checkpoints']];

/** Call the API; a refusal throws an Error that carries its body. */
async function call(method, path, body) {
  const opts = { control: true, admin: true, session: false, method, url: SERVER + path, timeout: 90000 };
  if (body !== undefined) {
    opts.headers = { 'Content-Type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  const r = await send(opts);
  if (r.error) throw new Error(r.error);
  if (!r.ok) {
    const j = r.json || {};
    const e = new Error(j.error || `${r.status} ${r.statusText}`);
    e.body = j;
    throw e;
  }
  return r.json !== undefined ? r.json : r.text;
}

/** What a pack creates, would create, holds or removed: a line per kind. */
function contents(byKind) {
  const lines = KINDS.map(([key, label]) => {
    const names = (byKind && byKind[key]) || [];
    return names.length ? h('li', null, label + ': ', h('span', { class: 'mono' }, names.join(', '))) : null;
  }).filter(Boolean);
  return lines.length ? h('ul', { class: 'small' }, lines) : h('p', { class: 'muted small' }, 'Nothing.');
}

/** A refusal with the names it carries (what is taken, what is wrong). */
function refusal(e) {
  const b = e.body || {};
  if (b.reason === 'permission_required') return notice('err', `This needs the ${b.permission} permission, which your roles do not grant.`);
  const items = (b.collisions || []).map((c) => h('li', null, c.kind.replace(/_/g, ' ') + ' ', h('span', { class: 'mono' }, c.name), ' exists'))
    .concat((b.problems || []).map((p) => h('li', null, p)));
  return h('div', { class: 'stack-sm' }, notice('err', e.message), items.length ? h('ul', { class: 'small' }, items) : null);
}

export function packsPanel() {
  const el = h('div', { class: 'stack' });
  const listEl = h('div', { id: 'pack-list' }, spinner('Loading the applied packs'));
  const listResult = h('div', { id: 'pack-list-result' });
  const text = h('textarea', { class: 'textarea mono', id: 'pack-document', rows: 12, spellcheck: 'false',
    placeholder: 'pack:\n  name: acme\n  title: Acme checkout\n  customer: Acme Corp\nmocks:\n  - name: acme-orders\n    spec_inline: { openapi: 3.0.3, ... }\nstoryboards:\n  - name: acme-demo\n    steps: [{ title: Welcome }]' });
  const file = h('input', { type: 'file', id: 'pack-file', accept: '.yaml,.yml,.json', class: 'input' });
  const untrusted = checkbox('From someone else: apply it as untrusted (no new gateways or webhooks)', false);
  untrusted.input.id = 'pack-untrusted';
  const previewBtn = perms.guard(h('button', { class: 'btn', type: 'button', id: 'pack-preview' }, 'Preview'), 'config.apply');
  const applyBtn = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'pack-apply' }, icon('upload'), 'Apply'), 'config.apply');
  const applyResult = h('div', { id: 'pack-result' });

  file.onchange = async () => {
    const f = file.files && file.files[0];
    if (!f) return;
    if (f.size > 4 * 1024 * 1024) {
      replace(applyResult, notice('err', 'That file is over 4 MB: a pack is a rustybin.yaml document.'));
      return;
    }
    text.value = await f.text();
  };

  async function load() {
    let d;
    try {
      d = await getJson(BASE, { session: false });
    } catch (e) {
      replace(listEl, /^40[13] /.test(e.message)
        ? notice('', 'The applied packs need the config.export permission.')
        : notice('err', 'The packs could not be loaded: ' + e.message));
      return;
    }
    if (!d.packs.length) {
      replace(listEl, h('p', { class: 'muted' }, 'No pack is applied.'));
      return;
    }
    replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'pack-table' },
      h('thead', null, h('tr', null, h('th', null, 'Pack'), h('th', null, 'Customer'), h('th', null, 'Applied'), h('th', null, 'Holds'), h('th', null, ''))),
      h('tbody', null, d.packs.map((p) => {
        const count = KINDS.reduce((n, [k]) => n + ((p.holds && p.holds[k]) || []).length, 0);
        return h('tr', { 'data-pack': p.name },
          h('td', null, h('div', { class: 'mono small' }, p.name), p.title ? h('div', { class: 'small' }, p.title) : null),
          h('td', { class: 'small' }, p.customer || ''),
          h('td', { class: 'small' }, fmtTime(p.applied_at), h('div', { class: 'muted' }, p.applied_by || '')),
          h('td', { class: 'small' }, badge(count + ' resource' + (count === 1 ? '' : 's'), 'outline'), p.notes ? h('div', { class: 'muted' }, p.notes) : null),
          h('td', null, h('div', { class: 'row nowrap' },
            perms.guard(h('button', { class: 'btn sm', type: 'button', 'data-pack-preview': p.name, onclick: () => teardown(p.name, true) }, 'What it holds'), 'config.apply'),
            h('button', { class: 'btn sm', type: 'button', title: 'Export as a pack', 'data-pack-export': p.name, onclick: () => exportPack(p.name) }, icon('download')),
            perms.guard(h('button', { class: 'btn sm', type: 'button', 'data-pack-teardown': p.name, onclick: () => teardown(p.name, false) }, icon('trash'), 'Tear down'), 'config.apply'))));
      })))));
  }

  async function run(dry) {
    if (!text.value.trim()) {
      replace(applyResult, notice('err', 'Paste a pack or choose its file.'));
      return;
    }
    replace(applyResult, spinner(dry ? 'Checking the pack' : 'Applying the pack'));
    try {
      const r = await call('POST', BASE + '/apply', { document: text.value, dry_run: dry, untrusted: untrusted.input.checked });
      replace(applyResult, h('div', { class: 'stack-sm', id: 'pack-applied' },
        notice(dry ? 'info' : 'ok', dry
          ? `The pack ${r.project} would create a project of that name with:`
          : `Applied ${r.project}: a project of that name now holds:`),
        contents(dry ? r.would_create : r.created),
        r.console_title ? h('p', { class: 'small' }, (dry ? 'The console title would become ' : 'The console title is now ') + JSON.stringify(r.console_title) + ' until the teardown.') : null));
      if (!dry) {
        toast('Applied ' + r.project);
        await load();
      }
    } catch (e) {
      replace(applyResult, refusal(e));
    }
  }

  async function teardown(name, dry) {
    if (!dry && !window.confirm(`Tear down the pack ${name}? Everything its project holds is deleted: mocks with their portal items, saved tests, storyboards, checkpoints, its schedules and the project.`)) return;
    try {
      const r = await call('DELETE', `${BASE}/${encodeURIComponent(name)}${dry ? '?dry_run=true' : ''}`);
      replace(listResult, h('div', { class: 'stack-sm', id: 'pack-teardown-result' },
        notice(dry ? 'info' : (r.ok ? 'ok' : 'warn'), dry ? `A teardown of ${name} would remove:` : `Tore down ${name}. Removed:`),
        contents(dry ? r.would_remove : r.removed),
        h('p', { class: 'small' }, 'Console title: ' + r.console_title + '.'),
        ...(r.problems || []).map((p) => notice('warn', p))));
      if (!dry) {
        toast('Tore down ' + name);
        await load();
      }
    } catch (e) {
      replace(listResult, refusal(e));
    }
  }

  async function exportPack(name) {
    try {
      const yaml = await call('GET', `${BASE}/${encodeURIComponent(name)}/export`);
      const a = h('a', { href: URL.createObjectURL(new Blob([yaml], { type: 'application/yaml' })), download: name + '-pack.yaml' });
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    } catch (e) {
      replace(listResult, refusal(e));
    }
  }

  previewBtn.onclick = () => run(true);
  applyBtn.onclick = () => run(false);

  el.append(
    card('Applied packs', { hint: 'GET ' + BASE },
      h('p', { class: 'muted small' }, 'A pack is one customer\'s demo: its mocks, their portal items, saved tests, schedules and storyboards, in a project named after it. Tearing it down removes what that project holds and nothing else.'),
      listEl, listResult),
    card('Apply a pack', { hint: 'POST ' + BASE + '/apply' },
      h('label', { class: 'field' }, h('span', null, 'A pack file (rustybin.yaml with a pack: header)'), file),
      h('label', { class: 'field' }, h('span', null, 'Or paste it'), text),
      untrusted.el,
      h('div', { class: 'row' }, previewBtn, applyBtn),
      h('p', { class: 'muted small' }, 'A name that already exists is refused, never overwritten. A pack whose mocks name spec files is applied with rustybin pack apply <directory>, which sends the specs inline. Any project exports as a pack: rustybin pack export <project>.'),
      applyResult));
  load();
  return el;
}
