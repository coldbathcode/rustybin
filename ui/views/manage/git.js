// Management, Source control: the repository form (the git.* settings), the
// status (GET /_rustybin/git: remote, head, ahead and behind, the drift
// between the working copy and the instance), Connect (POST
// /_rustybin/git/init), Commit and push, Pull (a dry run with the diff,
// then apply), Resolve a diverged branch, the deploy key, the history and
// the timeline of operations. "Write the commit message" asks the AI from
// the drift summary.
import { h, replace, card, input, checkbox, notice, toast, icon, badge, spinner, kvgrid, fmtTime, copyButton } from '../../lib/dom.js';
import { SERVER, getJson, send } from '../../lib/http.js';
import { settingsForm } from '../../lib/settingsform.js';
import * as assist from '../../lib/assist.js';
import { errorText } from '../../lib/mockapi.js';

async function post(path, body) {
  return send({ method: 'POST', url: SERVER + path, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body || {}), admin: true, session: false, timeout: 120000 });
}

function lines(list, id) {
  return h('ul', { class: 'small mono', id, style: { margin: 0, paddingLeft: '18px' } }, ...list.map((l) => h('li', null, l)));
}

function reportBlock(report) {
  if (!report) return null;
  const summary = report.summary || [];
  const problems = report.problems || [];
  return h('div', { class: 'stack' },
    summary.length ? lines(summary, 'git-diff') : h('p', { class: 'muted small', style: { margin: 0 } }, 'No changes.'),
    problems.length ? notice('warn', h('div', { class: 'grow' }, h('strong', null, 'Problems: '), lines(problems))) : null);
}

export function gitPanel(doc, reload, error) {
  const el = h('div', { class: 'stack' });
  const statusEl = h('div', { id: 'git-status' }, spinner('Reading the status'));
  const actionsEl = h('div', { id: 'git-actions' });
  const historyEl = h('div', { id: 'git-history' });
  const timelineEl = h('div', { id: 'git-timeline' });
  const keyEl = h('div', { id: 'git-key' });
  let st = null;
  let fetchNext = false;

  async function load(fetch) {
    try {
      st = await getJson('/_rustybin/git' + (fetch ? '?fetch=true' : ''), { admin: true, session: false });
    } catch (e) {
      replace(statusEl, notice('err', 'Source control could not be read: ' + (e.message || e)));
      return;
    }
    renderStatus();
    renderActions();
    renderKey();
    renderTimeline();
    loadHistory();
  }

  function renderStatus() {
    const drift = st.drift || {};
    const driftText = drift.error ? badge('unreadable: ' + drift.error, 'err')
      : drift.missing_document ? badge('no document in the working copy yet', 'warn')
        : (drift.changes || []).length ? badge((drift.changes || []).length + ' change(s) to commit', 'warn') : badge('matches the instance', 'ok');
    const head = st.head ? h('span', null, h('span', { class: 'mono' }, st.head.short), ' ', st.head.subject, h('span', { class: 'muted small' }, ' by ' + st.head.author + ', ' + fmtTime(st.head.at))) : 'none';
    const sync = st.upstream ? (st.ahead || st.behind ? badge((st.ahead ? st.ahead + ' ahead' : '') + (st.ahead && st.behind ? ', ' : '') + (st.behind ? st.behind + ' behind' : ''), st.ahead && st.behind ? 'err' : 'warn') : badge('in sync', 'ok'))
      : (st.initialised ? badge('the remote branch does not exist yet', 'warn') : null);
    const items = [];
    if (!st.git.available) items.push(notice('err', h('div', { class: 'grow' }, h('strong', null, 'git is not installed on this instance. '), 'The container image includes it; elsewhere put git on the PATH.')));
    if (!st.state_dir) items.push(notice('warn', h('div', { class: 'grow' }, h('strong', null, 'No state directory. '), 'Source control keeps its working copy in <state>/repo: set RUSTYBIN_STATE_DIR.')));
    if (st.remote_error) items.push(notice('err', 'git.remote: ' + st.remote_error));
    if (st.fetch_error) items.push(notice('warn', 'The remote could not be reached: ' + st.fetch_error));
    items.push(kvgrid([
      ['Repository', st.remote ? h('span', null, h('span', { class: 'mono' }, st.remote.url), h('span', { class: 'muted small' }, ' (' + st.remote.kind + (st.remote.host ? ', ' + st.remote.host : '') + ')')) : 'none configured'],
      ['Branch and directory', h('span', { class: 'mono' }, st.branch + ' / ' + (st.path || '(root)'))],
      ['Authentication', st.auth === 'deploy_key' ? 'deploy key' + (st.deploy_key ? '' : ' (not generated yet)') : 'token' + (st.token_set ? ' (set)' : ' (none: a public https remote can be pulled, not pushed)')],
      ['Working copy', st.initialised ? h('span', null, h('span', { class: 'mono' }, st.working_copy), ' ', driftText) : 'not connected yet'],
      ['Last commit', head],
      ['Against the remote', sync || (st.fetched ? 'unknown' : h('button', { class: 'btn sm ghost', type: 'button', id: 'git-fetch', onclick: () => load(true) }, icon('refresh'), 'Ask the remote'))],
      ['Automatic mode', st.auto === 'off' ? 'off' : st.auto === 'pull' ? 'pull and apply every ' + st.pull_every : 'push every change (checked every 30 s)'],
    ]));
    if ((drift.summary || []).length) items.push(h('div', null, h('h3', { class: 'section-title' }, 'What a commit would record'), lines(drift.summary, 'git-drift')));
    replace(statusEl, ...items);
  }

  function renderActions() {
    const ready = st.git.available && st.state_dir && st.remote && !st.remote_error;
    const message = input({ id: 'git-message', placeholder: 'What changed and why', 'aria-label': 'Commit message', maxlength: 2000 });
    const result = h('div', { id: 'git-result' });
    const prune = checkbox('Prune when applying (the repository is the whole truth)', true);
    prune.input.id = 'git-prune';
    const driftLines = (st.drift && st.drift.summary) || [];
    const writeBtn = assist.assistButton('Write the commit message', 'free_answer', () => ({
      question: 'Write a one-line git commit message (imperative mood, at most 72 characters, no quotes, no trailing period) for these configuration changes of a Rustybin instance:\n' + (driftLines.length ? driftLines.map((l) => '- ' + l).join('\n') : '- (no changes listed)'),
      view: 'manage',
    }), { class: 'ghost' });
    async function act(label, path, body, after) {
      replace(result, spinner(label));
      const r = await post(path, body);
      if (!r.ok) {
        const j = r.json || {};
        const conflict = j.conflict ? h('div', { class: 'row', style: { gap: '6px', marginTop: '6px', flexWrap: 'wrap' } },
          h('button', { class: 'btn sm', type: 'button', id: 'git-resolve-instance', onclick: () => act('Resolving', '/_rustybin/git/resolve', { with: 'instance', message: message.value }) }, 'Replace the repository\'s copy with this instance\'s'),
          h('button', { class: 'btn sm', type: 'button', id: 'git-resolve-repository', onclick: () => { if (confirm('Replace this instance\'s configuration with the repository\'s (with prune)?')) act('Resolving', '/_rustybin/git/resolve', { with: 'repository' }); } }, 'Replace this instance with the repository\'s')) : null;
        replace(result, notice('err', h('div', { class: 'grow' }, errorText(r, label), conflict)));
        load(false);
        return;
      }
      const j = r.json || {};
      replace(result, notice('ok', icon('check'), h('div', { class: 'grow' }, j.detail || label + ' done')), reportBlock(j.applied));
      if (after) after(j);
      load(false);
    }
    const connect = h('button', { class: 'btn primary', type: 'button', id: 'git-init', disabled: !ready, onclick: () => act('Connecting', '/_rustybin/git/init') }, icon('link'), st.initialised ? 'Reconnect' : 'Connect');
    const commit = h('button', { class: 'btn', type: 'button', id: 'git-commit', disabled: !ready || !st.initialised, onclick: () => act('Committing', '/_rustybin/git/commit', { message: message.value }) }, icon('check'), 'Commit');
    const push = h('button', { class: 'btn', type: 'button', id: 'git-push', disabled: !ready || !st.initialised, onclick: () => act('Pushing', '/_rustybin/git/push') }, icon('upload'), 'Push');
    const commitPush = h('button', { class: 'btn primary', type: 'button', id: 'git-commit-push', disabled: !ready || !st.initialised, onclick: () => act('Committing', '/_rustybin/git/commit', { message: message.value }, (j) => { if (j.committed) act('Pushing', '/_rustybin/git/push'); }) }, icon('upload'), 'Commit and push');
    const pullDry = h('button', { class: 'btn', type: 'button', id: 'git-pull-dry', disabled: !ready || !st.initialised, onclick: () => act('Checking', '/_rustybin/git/pull', { apply: true, dry_run: true, prune: prune.input.checked }) }, icon('diff'), 'Pull, dry run');
    const pullApply = h('button', { class: 'btn primary', type: 'button', id: 'git-pull-apply', disabled: !ready || !st.initialised, onclick: () => { if (confirm('Pull and apply the repository\'s configuration' + (prune.input.checked ? ' with prune' : '') + '?')) act('Pulling', '/_rustybin/git/pull', { apply: true, dry_run: false, prune: prune.input.checked }); } }, icon('download'), 'Pull and apply');
    replace(actionsEl,
      card('Connect', { hint: st.initialised ? 'the working copy exists' : 'clone the branch, or start one for an empty repository' },
        h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, connect, h('span', { class: 'muted small' }, ready ? 'Clones ' + st.remote.url + ' (' + st.branch + ') into the state directory.' : 'Save a repository above first.'))),
      card('Commit and push', { hint: 'export the instance into the working copy, commit, push' },
        h('div', { class: 'stack' },
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, h('div', { class: 'grow' }, message), writeBtn),
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, commitPush, commit, push))),
      card('Pull', { hint: 'fetch, fast-forward, apply the repository\'s document' },
        h('div', { class: 'stack' },
          prune.el,
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, pullDry, pullApply, h('span', { class: 'muted small' }, 'The instance never merges: a diverged branch offers the two ways out.')))),
      result);
  }

  function renderKey() {
    const key = st.deploy_key;
    const gen = h('button', { class: 'btn sm', type: 'button', id: 'git-key-generate', disabled: !st.state_dir, onclick: async () => {
      if (key && !confirm('Generate a new deploy key? The old public key stops working.')) return;
      const r = await post('/_rustybin/git/key', { replace: !!key });
      if (!r.ok) { toast(errorText(r, 'key'), 'err'); return; }
      toast('Deploy key generated');
      load(false);
    } }, icon('token'), key ? 'Generate a new key' : 'Generate the deploy key');
    replace(keyEl, card('Deploy key', { hint: 'ed25519, for ssh remotes; add the public key to the repository', actions: gen },
      key ? h('div', { class: 'stack' },
        h('div', { class: 'row', style: { gap: '6px' } }, h('code', { class: 'mono small', id: 'git-public-key', style: { wordBreak: 'break-all' } }, key.public), copyButton(() => key.public, { label: 'Copy', toast: 'Public key copied' })),
        h('p', { class: 'muted small', style: { margin: 0 } }, 'The private half stays in ' + key.path + ' (mode 0600). Give the key write access for push.'))
        : h('p', { class: 'muted small', style: { margin: 0 } }, 'No key yet. With git.auth set to deploy_key, generate one here and add its public key to the repository (a deploy key on GitHub or GitLab, with write access to push).')));
  }

  function renderTimeline() {
    const list = st.timeline || [];
    replace(timelineEl, card('Operations', { hint: 'the last ' + list.length + ' on this instance' },
      list.length ? h('table', { class: 'table small' }, h('thead', null, h('tr', null, h('th', null, 'When'), h('th', null, 'Operation'), h('th', null, 'By'), h('th', null, 'Outcome'))),
        h('tbody', null, ...list.map((o) => h('tr', null, h('td', { class: 'muted' }, fmtTime(o.at)), h('td', null, o.op), h('td', null, o.by), h('td', null, badge(o.ok ? 'ok' : 'failed', o.ok ? 'ok' : 'err'), ' ', o.detail)))))
        : h('p', { class: 'muted small', style: { margin: 0 } }, 'Nothing yet.')));
  }

  async function loadHistory() {
    if (!st.initialised) { replace(historyEl); return; }
    const r = await send({ url: SERVER + '/_rustybin/git/history', admin: true, session: false });
    const commits = (r.ok && r.json && r.json.commits) || [];
    replace(historyEl, card('History', { hint: commits.length ? 'the last ' + commits.length + ' commits' : 'no commits yet' },
      commits.length ? h('table', { class: 'table small' }, h('thead', null, h('tr', null, h('th', null, 'Commit'), h('th', null, 'Message'), h('th', null, 'Author'), h('th', null, 'When'), h('th', null, 'Files'))),
        h('tbody', null, ...commits.map((c) => h('tr', null, h('td', { class: 'mono' }, c.short), h('td', null, c.subject), h('td', null, c.author), h('td', { class: 'muted' }, fmtTime(c.at)), h('td', { class: 'muted' }, c.stat)))))
        : h('p', { class: 'muted small', style: { margin: 0 } }, r.ok ? 'Commit the instance to start the history.' : errorText(r, 'history'))));
  }

  if (doc) {
    const form = settingsForm(doc, 'git', { saveLabel: 'Save repository settings', onSaved: (d) => { reload(d); load(false); } });
    el.appendChild(card('Repository', { hint: 'where this instance commits its configuration' }, form.el));
  } else {
    el.appendChild(card('Repository', {}, notice('err', 'Settings could not be loaded: ' + ((error && error.message) || error))));
  }
  el.appendChild(card('Status', { hint: 'the working copy against the instance and the remote', actions: h('button', { class: 'btn sm ghost', type: 'button', id: 'git-refresh', onclick: () => load(fetchNext) }, icon('refresh'), 'Refresh') }, statusEl));
  el.appendChild(actionsEl);
  el.appendChild(keyEl);
  el.appendChild(historyEl);
  el.appendChild(timelineEl);
  fetchNext = true;
  load(false);
  return el;
}
