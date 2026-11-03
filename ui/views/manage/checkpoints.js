// Management, Checkpoints: save the state a demo starts from (the instance's
// API mocks with their data, the health toggle) and restore it before a run.
// GET /_rustybin/checkpoints; saving, restoring and deleting are admin. "In
// the repository" lists and restores the configuration repository's
// (?from=git, admin). The same on every surface: rustybin checkpoint, the
// *_checkpoint(s) tools.
import { h, replace, card, input, checkbox, notice, toast, icon, badge, spinner, fmtTime } from '../../lib/dom.js';
import { SERVER, getJson, send } from '../../lib/http.js';
import * as perms from '../../lib/perms.js';
import * as projects from '../../lib/projects.js';

const BASE = '/_rustybin/checkpoints';

async function call(method, path) {
  const r = await send({ control: true, admin: true, session: false, method, url: SERVER + path });
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
  return r.json;
}

function changesView(r, id = 'checkpoint-changes') {
  const m = r.mocks;
  const line = (label, names) => names.length ? h('li', null, label + ': ', h('span', { class: 'mono' }, names.join(', '))) : null;
  return h('div', { class: 'stack-sm', id },
    notice(r.dry_run ? 'info' : 'ok', r.dry_run ? `A restore of ${r.checkpoint} would change:` : (r.changed ? `Restored ${r.checkpoint}.` : `Nothing to change: the instance is ${r.checkpoint}.`)),
    h('ul', { class: 'small' },
      line('created', m.created), line('replaced', m.replaced), line('data restored', m.data_restored),
      line('deleted', m.deleted), line('unchanged', m.unchanged),
      h('li', null, `health: ${r.health.before} to ${r.health.after}; flaky counters ${r.dry_run ? 'to reset' : 'reset'}: ${r.flaky_counters_reset}`)),
    ...(r.problems || []).map((p) => notice('warn', p)));
}

export function checkpointsPanel() {
  const el = h('div', { class: 'stack' });
  const listEl = h('div', { id: 'checkpoint-list' }, spinner('Loading the checkpoints'));
  const resultEl = h('div', { id: 'checkpoint-result' });
  const repoEl = h('div', { id: 'checkpoint-repo' }, spinner("Reading the repository's checkpoints"));
  const repoResultEl = h('div', { id: 'checkpoint-repo-result' });
  const name = input({ id: 'checkpoint-name', placeholder: 'demo-start' });
  const replaceBox = checkbox('Replace a checkpoint of that name', false);
  const saveBtn = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'checkpoint-save' }, icon('download'), 'Save checkpoint'), 'checkpoints.write');

  async function load() {
    try {
      const d = await getJson(BASE + projects.query(), { session: false });
      if (!d.checkpoints.length) {
        replace(listEl, h('p', { class: 'muted' }, 'No checkpoints yet.'));
        return;
      }
      replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'checkpoint-table' },
        h('thead', null, h('tr', null, h('th', null, 'Name'), h('th', null, 'Saved'), h('th', null, 'Mocks'), h('th', null, 'Health'), h('th', null, ''))),
        h('tbody', null, d.checkpoints.map((c) => h('tr', { 'data-checkpoint': c.name },
          h('td', { class: 'mono small' }, c.name),
          h('td', { class: 'small' }, fmtTime(c.saved_at)),
          h('td', { class: 'small mono' }, c.mocks.join(', ') || 'none'),
          h('td', null, badge(c.healthy ? 'healthy' : 'unhealthy', c.healthy ? 'ok' : 'warn')),
          h('td', null, h('div', { class: 'row nowrap' },
            h('button', { class: 'btn sm', type: 'button', 'data-preview': c.name, onclick: () => restore(c.name, true) }, 'Preview'),
            perms.guard(h('button', { class: 'btn sm', type: 'button', 'data-restore': c.name, onclick: () => restore(c.name, false) }, icon('history'), 'Restore'), 'checkpoints.restore'),
            perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'data-delete': c.name, onclick: () => remove(c.name) }, icon('trash')), 'checkpoints.write')))))))));
    } catch (e) {
      replace(listEl, notice('err', 'The checkpoints could not be loaded: ' + e.message));
    }
  }

  async function restore(n, dry, fromGit) {
    if (!dry && !window.confirm(`Restore ${n}${fromGit ? ' from the repository' : ''}? The instance's API mocks become what it saved (others are deleted), the health toggle is set and the flaky counters reset.`)) return;
    const query = [dry ? 'dry_run=true' : '', fromGit ? 'from=git' : ''].filter(Boolean).join('&');
    const target = fromGit ? repoResultEl : resultEl;
    try {
      replace(target, changesView(await call('POST', `${BASE}/${encodeURIComponent(n)}/restore${query ? '?' + query : ''}`), fromGit ? 'checkpoint-repo-changes' : 'checkpoint-changes'));
    } catch (e) {
      replace(target, notice('err', e.message));
    }
  }

  // The configuration repository's checkpoints (GET ?from=git, admin): the
  // files a commit wrote next to rustybin.yaml, at the working copy's HEAD.
  async function loadRepo() {
    const r = await send({ control: true, admin: true, session: false, url: SERVER + BASE + '?from=git' });
    if (!r.ok) {
      const why = (r.json && r.json.error) || r.error || `${r.status} ${r.statusText}`;
      replace(repoEl, h('p', { class: 'muted small', id: 'checkpoint-repo-off' }, 'Source control is not ready: ' + why + ' (Management, Source control).'));
      return;
    }
    const d = r.json;
    if (!d.checkpoints.length) {
      replace(repoEl, h('p', { class: 'muted small', id: 'checkpoint-repo-empty' }, `No checkpoints in ${d.path} at the working copy's last commit. Commit one from Source control (Commit with checkpoints), push, then pull on the instance that restores it.`));
      return;
    }
    replace(repoEl, h('div', { class: 'stack-sm' },
      d.head ? h('p', { class: 'muted small', style: { margin: 0 } }, `${d.path} at `, h('span', { class: 'mono' }, d.head.short), ' ', d.head.subject) : null,
      h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'checkpoint-repo-table' },
        h('thead', null, h('tr', null, h('th', null, 'Name'), h('th', null, 'Saved'), h('th', null, 'Mocks'), h('th', null, 'Health'), h('th', null, ''))),
        h('tbody', null, d.checkpoints.map((c) => c.ok ? h('tr', { 'data-repo-checkpoint': c.name },
          h('td', { class: 'mono small' }, c.name),
          h('td', { class: 'small' }, fmtTime(c.saved_at)),
          h('td', { class: 'small mono' }, c.mocks.join(', ') || 'none'),
          h('td', null, badge(c.healthy ? 'healthy' : 'unhealthy', c.healthy ? 'ok' : 'warn')),
          h('td', null, h('div', { class: 'row nowrap' },
            h('button', { class: 'btn sm', type: 'button', 'data-repo-preview': c.name, onclick: () => restore(c.name, true, true) }, 'Preview'),
            perms.guard(h('button', { class: 'btn sm', type: 'button', 'data-repo-restore': c.name, onclick: () => restore(c.name, false, true) }, icon('history'), 'Restore'), 'checkpoints.restore'))))
          : h('tr', { 'data-repo-checkpoint': c.name },
            h('td', { class: 'mono small' }, c.name),
            h('td', { colspan: 4 }, badge('cannot be restored', 'err'), ' ', h('span', { class: 'small muted' }, c.error))))))),
      d.truncated ? notice('info', `Only the first ${d.max_checkpoints} checkpoint files are listed; a later one is restored by name (rustybin checkpoint restore NAME --from-git).`) : null));
  }

  async function remove(n) {
    if (!window.confirm(`Delete the checkpoint ${n}? The instance is not changed.`)) return;
    try {
      await call('DELETE', `${BASE}/${encodeURIComponent(n)}`);
      toast('Deleted ' + n);
      load();
    } catch (e) {
      toast('Could not delete: ' + e.message, 'err');
    }
  }

  saveBtn.onclick = async () => {
    const n = name.value.trim();
    if (!n) { toast('Give the checkpoint a name', 'err'); return; }
    const r = await send({ control: true, admin: true, session: false, method: 'POST', url: SERVER + BASE, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: n, replace: replaceBox.input.checked, project: projects.forNew() }) });
    if (!r.ok) { toast('Not saved: ' + ((r.json && r.json.error) || r.error || r.status), 'err'); return; }
    toast(`Saved ${n} (${r.json.mocks.length} mock(s))`);
    load();
  };

  el.appendChild(card('Save a checkpoint', { hint: 'POST ' + BASE },
    h('p', { class: 'muted small' }, "A checkpoint holds the instance's API mocks (the deployed revision, a draft that differs, on or off, their stateful data) and the health toggle. Never visitors' or keys' mocks, file mocks, settings or secrets."),
    h('div', { class: 'row' }, h('label', { class: 'field grow' }, h('span', null, 'Name'), name), replaceBox.el, saveBtn)));
  el.appendChild(card('Checkpoints', { hint: 'Restore before a demo run' }, listEl, resultEl));
  el.appendChild(card('In the repository', { hint: 'GET ' + BASE + '?from=git', actions: h('button', { class: 'btn sm ghost', type: 'button', id: 'checkpoint-repo-refresh', onclick: () => loadRepo() }, icon('refresh'), 'Refresh') },
    h('p', { class: 'muted small' }, "Checkpoints committed next to rustybin.yaml (only when a commit names them, never by the automatic mode). Read from the working copy's last commit, so pull first; each is checked like a local one before a restore."),
    repoEl, repoResultEl));
  load();
  loadRepo();
  return el;
}
