// Management, Source control: the repository form (the git.* settings), the
// status (GET /_rustybin/git: remote, head, ahead and behind, the drift
// between the working copy and the instance), Connect (POST
// /_rustybin/git/init), Commit and push (with a choice of checkpoints to
// commit next to the document), Pull (a dry run with the diff,
// then apply), a diverged branch (what differs, and the three ways out,
// each with a preview: this instance's copy, the repository's, or a new
// branch for a pull request at the git host, which the host's API opens
// when the box is ticked: its address is shown after), Branches (the remote's, a
// switch with a preview, a new branch from this instance), the deploy key,
// the commit signing key (its public half and fingerprint), verified pulls
// (the switch, the allowed signers, the last verified head, the last
// refusal), the history and the timeline of operations. "Write the commit message" asks the AI from
// the drift summary.
import { h, replace, card, input, checkbox, notice, toast, icon, badge, spinner, kvgrid, fmtTime, copyButton } from '../../lib/dom.js';
import { SERVER, getJson, send } from '../../lib/http.js';
import { settingsForm } from '../../lib/settingsform.js';
import * as assist from '../../lib/assist.js';
import { errorText } from '../../lib/mockapi.js';
import * as perms from '../../lib/perms.js';

// One git operation at a time: the console's own reads (the status, the
// history) hold the working copy for a moment. A call that finds it taken
// (409 with busy) waits and asks again rather than fail what the person
// clicked or draw a card that says "busy".
async function unlessBusy(once) {
  let r = await once();
  for (let i = 0; i < 12 && !r.ok && r.json && r.json.busy; i++) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    r = await once();
  }
  return r;
}

async function post(path, body) {
  return unlessBusy(() => send({ method: 'POST', url: SERVER + path, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body || {}), admin: true, session: false, timeout: 120000 }));
}

// A read of a control path of the console's own (a literal of this file).
async function read(path) {
  return unlessBusy(() => send({ control: true, url: SERVER + path, admin: true, session: false, timeout: 120000 }));
}

// A refused commit of the repository's side (verification.last_refusal).
function refusalText(r) {
  return (r.commit ? 'commit ' + String(r.commit).slice(0, 7) + ' ' : '') + (r.why || 'is not verified') + (r.branch ? ' (' + r.branch + ', ' + fmtTime(r.at) + ')' : '');
}

// The pull request a git host opened (untrusted_pull_request: the host's
// own answer). Its address is a link only when it is a plain web address;
// it is shown as text either way.
function pullRequestBlock(pr) {
  const url = typeof pr.url === 'string' && /^https?:\/\//i.test(pr.url) ? pr.url : null;
  return h('div', { class: 'small', id: 'git-pull-request' },
    h('strong', null, 'Pull request' + (Number.isInteger(pr.number) ? ' ' + pr.number : '') + (url ? ': ' : '')),
    url ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer', class: 'mono' }, url) : null,
    h('span', { class: 'muted' }, ' (as the git host answered it)'));
}

function lines(list, id) {
  return h('ul', { class: 'small mono', id, style: { margin: 0, paddingLeft: '18px' } }, ...list.map((l) => h('li', null, l)));
}

// What differs on a diverged branch (GET /_rustybin/git: divergence): the
// commits of each side and what each way out would change, by section.
function divergenceBlock(d) {
  const side = (title, list, id) => h('div', null, h('div', { class: 'small' }, h('strong', null, title)),
    (list || []).length ? lines(list, id) : h('p', { class: 'muted small', style: { margin: 0 } }, 'Nothing.'));
  return notice('warn', h('div', { class: 'grow stack', id: 'git-divergence' },
    h('div', null, h('strong', null, 'The branch has diverged. '), d.ahead + ' commit(s) here, ' + d.behind + ' on the remote. The instance never merges and the automatic mode does not resolve it: a person chooses a way out below.'),
    d.error ? h('p', { class: 'small', style: { margin: 0 } }, 'What differs could not be read: ' + d.error) : null,
    side('Commits here', d.local_commits, 'git-diverged-local'),
    side('Commits on the remote', d.remote_commits, 'git-diverged-remote'),
    d.instance ? side('With this instance\'s copy, the repository\'s document changes', d.instance.summary, 'git-diverged-instance') : null,
    d.repository ? side('With the repository\'s copy, this instance changes', d.repository.summary, 'git-diverged-repository') : null));
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
  const branchesEl = h('div', { id: 'git-branches' });
  const signingEl = h('div', { id: 'git-signing' });
  const verifyEl = h('div', { id: 'git-verify' });
  // What the last action answered. These outlive the reload of the status
  // that follows an action (the cards around them are drawn again): the
  // answer of a pull request names the branch to open it from.
  const result = h('div', { id: 'git-result' });
  const branchResult = h('div', { id: 'git-branch-result' });
  // So does what a person typed, ticked or listed: every action reloads the
  // status and draws the cards again around these same controls.
  const message = input({ id: 'git-message', placeholder: 'What changed and why', 'aria-label': 'Commit message', maxlength: 2000 });
  const prBranch = input({ id: 'git-resolve-branch', placeholder: 'a new branch (a name is made when empty)', 'aria-label': 'Branch for the pull request', maxlength: 128 });
  const prOpen = checkbox('Open the pull request at the git host', false);
  prOpen.input.id = 'git-resolve-open';
  const branchList = h('div', { id: 'git-branch-list' });
  let branchesListed = false;
  const branchPrune = checkbox('Prune when applying (the branch\'s document is the whole truth)', true);
  branchPrune.input.id = 'git-branch-prune';
  const branchName = input({ id: 'git-branch-new', placeholder: 'staging', 'aria-label': 'New branch name', maxlength: 128 });
  let st = null;
  let fetchNext = false;
  let refreshSettings = async () => {};

  async function load(fetch, attempt) {
    let next;
    try {
      next = await getJson('/_rustybin/git' + (fetch ? '?fetch=true' : ''), { admin: true, session: false });
    } catch (e) {
      replace(statusEl, notice('err', 'Source control could not be read: ' + (e.message || e)));
      return;
    }
    // Another git operation holds the working copy (the history of the last
    // load, an automatic pull): that status has no last commit and no
    // divergence. Ask again shortly, and keep what is shown meanwhile.
    if (next.busy && (attempt || 0) < 8) {
      setTimeout(() => load(fetch, (attempt || 0) + 1), 250);
      if (st) return;
    }
    st = next;
    renderStatus();
    renderActions();
    renderBranches();
    renderKey();
    renderSigning();
    renderVerify();
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
      ['Git host', st.host && st.host.kind ? h('span', { id: 'git-host' }, st.host.kind, st.host.api ? h('span', { class: 'muted small' }, ' (' + st.host.api + (st.host.repository ? ', ' + st.host.repository : '') + ') ') : ' ', st.host.ready ? badge('can open a pull request', 'ok') : badge('cannot open a pull request yet', 'warn')) : h('span', { id: 'git-host' }, 'none: a pull request is opened by hand')],
      ['Commit signing', st.signing && st.signing.enabled ? (st.signing.problem ? badge('on, and cannot work', 'err') : badge('on', 'ok')) : 'off'],
      ['Verified pulls', st.verification && st.verification.enabled ? (st.verification.problem ? badge('on, and nothing can pass', 'err') : st.verification.last_refusal ? badge('on, the last pull was refused', 'err') : badge('on', 'ok')) : 'off'],
    ]));
    if ((drift.summary || []).length) items.push(h('div', null, h('h3', { class: 'section-title' }, 'What a commit would record'), lines(drift.summary, 'git-drift')));
    if (st.divergence) items.push(divergenceBlock(st.divergence));
    if (st.signing && st.signing.problem) items.push(notice('warn', h('div', { class: 'grow' }, h('strong', null, 'Commits cannot be signed. '), st.signing.problem)));
    if (st.verification && st.verification.enabled && st.verification.last_refusal) items.push(notice('err', h('div', { class: 'grow', id: 'git-unverified' }, h('strong', null, 'The repository\'s side is not verified, and nothing of it was applied. '), refusalText(st.verification.last_refusal))));
    replace(statusEl, ...items);
  }

  function renderActions() {
    const ready = st.git.available && st.state_dir && st.remote && !st.remote_error;
    const prune = checkbox('Prune when applying (the repository is the whole truth)', true);
    prune.input.id = 'git-prune';
    const driftLines = (st.drift && st.drift.summary) || [];
    const writeBtn = assist.assistButton('Write the commit message', 'free_answer', () => ({
      question: 'Write a one-line git commit message (imperative mood, at most 72 characters, no quotes, no trailing period) for these configuration changes of a Rustybin instance:\n' + (driftLines.length ? driftLines.map((l) => '- ' + l).join('\n') : '- (no changes listed)'),
      view: 'manage',
    }), { class: 'ghost' });
    // The ways out of a diverged branch, each a person's call with a
    // preview first: this instance's copy, the repository's, or this
    // instance's side on a new branch for a pull request at the git host.
    function waysOut() {
      const id = (name) => 'git-resolve-' + name;
      const resolve = (body) => act(body.dry_run ? 'Checking' : 'Resolving', '/_rustybin/git/resolve', body);
      // The git host's API opens the pull request only when it can (a
      // host is set up and has a token): the box says why when it cannot.
      const host = st.host || {};
      prOpen.input.disabled = !host.ready;
      if (!host.ready) prOpen.input.checked = false;
      const opens = () => !!host.ready && prOpen.input.checked;
      const pr = (dry) => resolve(Object.assign({ with: 'pull_request', dry_run: dry, message: message.value }, prBranch.value.trim() ? { branch: prBranch.value.trim() } : {}, opens() ? { open: true } : {}));
      return h('div', { class: 'stack', style: { marginTop: '6px' } },
        h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } },
          perms.guard(h('button', { class: 'btn sm ghost', type: 'button', id: id('instance-preview'), onclick: () => resolve({ with: 'instance', dry_run: true }) }, icon('diff'), 'Preview'), 'git.write'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', id: id('instance'), onclick: () => { if (confirm('Replace the repository\'s configuration with this instance\'s? The local commits are replaced by one commit on top of the remote branch, then pushed (never a forced push).')) resolve({ with: 'instance', message: message.value }); } }, 'Use this instance\'s copy'), 'git.write'),
          h('span', { class: 'muted small' }, 'The repository takes this instance\'s configuration.')),
        h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } },
          perms.guard(h('button', { class: 'btn sm ghost', type: 'button', id: id('repository-preview'), onclick: () => resolve({ with: 'repository', dry_run: true }) }, icon('diff'), 'Preview'), 'git.write'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', id: id('repository'), onclick: () => { if (confirm('Replace this instance\'s configuration with the repository\'s (with prune)? The local commits are dropped.')) resolve({ with: 'repository' }); } }, 'Use the repository\'s copy'), 'git.write'),
          h('span', { class: 'muted small' }, 'This instance takes the repository\'s configuration, with prune.')),
        h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } },
          perms.guard(h('button', { class: 'btn sm ghost', type: 'button', id: id('pr-preview'), onclick: () => pr(true) }, icon('diff'), 'Preview'), 'git.write'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', id: id('pr'), onclick: () => { if (confirm(opens() ? 'Push this instance\'s side to a new branch of the remote and ask the git host (' + host.kind + ') to open a pull request? Nothing is applied here, and nothing is merged.' : 'Push this instance\'s side to a new branch of the remote for a pull request? Nothing is applied here.')) pr(false); } }, icon('upload'), 'Push to a new branch for a pull request'), 'git.write'),
          h('div', { style: { minWidth: '220px' } }, prBranch),
          h('span', { class: 'muted small' }, 'A person merges it at the git host; then pull.')),
        h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } },
          prOpen.el,
          h('span', { class: 'muted small', id: 'git-resolve-open-hint' }, host.ready
            ? 'Asks ' + host.kind + ' (' + host.api + ') to open it from the new branch into ' + st.branch + '. It is never merged from here.'
            : 'Not possible yet: ' + (host.why || 'no git host is set up') + '. The settings git.host, git.host_api and git.host_token are in the repository form above.')));
    }
    async function act(label, path, body, after) {
      replace(result, spinner(label));
      const r = await post(path, body);
      if (!r.ok) {
        const j = r.json || {};
        // The divergence is shown in the status with the ways out in their
        // own card (the reload below draws both).
        replace(result, notice('err', h('div', { class: 'grow' }, errorText(r, label), j.conflict ? h('div', { class: 'small', id: 'git-conflict' }, 'The ways out are under "A diverged branch".') : null)));
        load(false);
        return;
      }
      const j = r.json || {};
      // A pull request the host did not open: the branch is pushed all the
      // same, so the answer is a warning, with the host's own words apart.
      const notOpened = j.opened === false && j.not_opened;
      replace(result, notice(notOpened ? 'warn' : 'ok', j.detail || label + ' done'),
        j.untrusted_pull_request ? pullRequestBlock(j.untrusted_pull_request) : null,
        notOpened && j.not_opened.untrusted_host_message ? h('div', { class: 'small', id: 'git-not-opened' }, h('strong', null, 'The git host said: '), j.not_opened.untrusted_host_message) : null,
        j.would_open ? h('div', { class: 'small', id: 'git-would-open' }, h('strong', null, 'Would open: '), j.would_open.title, h('span', { class: 'muted' }, ' at ' + j.would_open.host.api + ' (' + j.would_open.host.kind + ', ' + j.would_open.host.repository + '), from ' + j.would_open.head + ' into ' + j.would_open.base)) : null,
        (j.commits || []).length ? lines(j.commits, 'git-result-commits') : null,
        !j.applied && (j.summary || []).length ? lines(j.summary, 'git-result-summary') : null,
        reportBlock(j.applied));
      if (after) after(j);
      // A preview changes nothing: the status is as it was.
      if (!j.dry_run) load(false);
    }
    const connect = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'git-init', disabled: !ready, onclick: () => act('Connecting', '/_rustybin/git/init') }, icon('link'), st.initialised ? 'Reconnect' : 'Connect'), 'git.write');
    // The checkpoint choice: local checkpoints written next to rustybin.yaml
    // in this commit (mock data, so only when ticked; never automatic).
    const picked = new Set();
    const choiceEl = h('div', { id: 'git-checkpoints', class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, h('span', { class: 'muted small' }, 'Loading the checkpoints'));
    getJson('/_rustybin/checkpoints', { session: false }).then((d) => {
      const list = (d && d.checkpoints) || [];
      if (!list.length) {
        replace(choiceEl, h('span', { class: 'muted small' }, 'No checkpoints to commit (Management, Checkpoints saves one).'));
        return;
      }
      replace(choiceEl, h('span', { class: 'muted small' }, 'Also commit checkpoints (they hold mock data):'),
        ...list.map((c) => {
          const box = checkbox(c.name, false);
          box.input.setAttribute('data-git-checkpoint', c.name);
          box.input.addEventListener('change', () => { if (box.input.checked) picked.add(c.name); else picked.delete(c.name); });
          return box.el;
        }));
    }).catch((e) => replace(choiceEl, h('span', { class: 'muted small' }, 'The checkpoints could not be loaded: ' + (e.message || e))));
    const commitBody = () => (picked.size ? { message: message.value, checkpoints: [...picked] } : { message: message.value });
    const commit = perms.guard(h('button', { class: 'btn', type: 'button', id: 'git-commit', disabled: !ready || !st.initialised, onclick: () => act('Committing', '/_rustybin/git/commit', commitBody()) }, icon('check'), 'Commit'), 'git.write');
    const push = perms.guard(h('button', { class: 'btn', type: 'button', id: 'git-push', disabled: !ready || !st.initialised, onclick: () => act('Pushing', '/_rustybin/git/push') }, icon('upload'), 'Push'), 'git.write');
    const commitPush = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'git-commit-push', disabled: !ready || !st.initialised, onclick: () => act('Committing', '/_rustybin/git/commit', commitBody(), (j) => { if (j.committed) act('Pushing', '/_rustybin/git/push'); }) }, icon('upload'), 'Commit and push'), 'git.write');
    const pullDry = perms.guard(h('button', { class: 'btn', type: 'button', id: 'git-pull-dry', disabled: !ready || !st.initialised, onclick: () => act('Checking', '/_rustybin/git/pull', { apply: true, dry_run: true, prune: prune.input.checked }) }, icon('diff'), 'Pull, dry run'), 'git.write');
    const pullApply = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'git-pull-apply', disabled: !ready || !st.initialised, onclick: () => { if (confirm('Pull and apply the repository\'s configuration' + (prune.input.checked ? ' with prune' : '') + '?')) act('Pulling', '/_rustybin/git/pull', { apply: true, dry_run: false, prune: prune.input.checked }); } }, icon('download'), 'Pull and apply'), 'git.write');
    replace(actionsEl,
      card('Connect', { hint: st.initialised ? 'the working copy exists' : 'clone the branch, or start one for an empty repository' },
        h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, connect, h('span', { class: 'muted small' }, ready ? 'Clones ' + st.remote.url + ' (' + st.branch + ') into the state directory.' : 'Save a repository above first.'))),
      card('Commit and push', { hint: 'export the instance into the working copy, commit, push' },
        h('div', { class: 'stack' },
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, h('div', { class: 'grow' }, message), writeBtn),
          choiceEl,
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, commitPush, commit, push))),
      card('Pull', { hint: 'fetch, fast-forward, apply the repository\'s document' },
        h('div', { class: 'stack' },
          prune.el,
          h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, pullDry, pullApply, h('span', { class: 'muted small' }, 'The instance never merges: a diverged branch says what differs and offers the ways out.')))),
      st.divergence ? card('A diverged branch', { hint: 'never merged here: choose a way out, with a preview first' }, h('div', { id: 'git-ways-out' }, waysOut())) : null,
      result);
  }

  function renderKey() {
    const key = st.deploy_key;
    const gen = perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'git-key-generate', disabled: !st.state_dir, onclick: async () => {
      if (key && !confirm('Generate a new deploy key? The old public key stops working.')) return;
      const r = await post('/_rustybin/git/key', { replace: !!key });
      if (!r.ok) { toast(errorText(r, 'key'), 'err'); return; }
      toast('Deploy key generated');
      load(false);
    } }, icon('token'), key ? 'Generate a new key' : 'Generate the deploy key'), 'git.write');
    replace(keyEl, card('Deploy key', { hint: 'ed25519, for ssh remotes; add the public key to the repository', actions: gen },
      key ? h('div', { class: 'stack' },
        h('div', { class: 'row', style: { gap: '6px' } }, h('code', { class: 'mono small', id: 'git-public-key', style: { wordBreak: 'break-all' } }, key.public), copyButton(() => key.public, { label: 'Copy', toast: 'Public key copied' })),
        h('p', { class: 'muted small', style: { margin: 0 } }, 'The private half stays in ' + key.path + ' (mode 0600). Give the key write access for push.'))
        : h('p', { class: 'muted small', style: { margin: 0 } }, 'No key yet. With git.auth set to deploy_key, generate one here and add its public key to the repository (a deploy key on GitHub or GitLab, with write access to push).')));
  }

  // Branches: the remote's (asked on request, it reaches the remote), a
  // switch with a preview (what applying that branch's document would
  // change, validated like the apply) and a new branch from this instance.
  function renderBranches() {
    const ready = !!(st.git.available && st.state_dir && st.remote && !st.remote_error && st.initialised);
    const listEl = branchList;
    if (!ready) branchesListed = false;
    if (!branchesListed) replace(listEl, h('p', { class: 'muted small', style: { margin: 0 } }, ready ? 'Ask the remote for its branches to switch to one.' : 'Connect the repository first.'));
    const out = branchResult;
    const prune = branchPrune;
    const name = branchName;
    async function branch(label, body) {
      replace(out, spinner(label));
      const r = await post('/_rustybin/git/branch', body);
      if (!r.ok) { replace(out, notice('err', errorText(r, label))); return; }
      const j = r.json || {};
      replace(out, notice('ok', j.detail || label + ' done'),
        !j.applied && (j.summary || []).length ? lines(j.summary, 'git-branch-summary') : null,
        reportBlock(j.applied));
      if (!j.dry_run) {
        refreshSettings();
        load(false);
        // The list shown is of before the switch: ask the remote again.
        if (branchesListed) list();
      }
    }
    async function list() {
      replace(listEl, spinner('Asking the remote'));
      const r = await read('/_rustybin/git/branches');
      if (!r.ok) {
        replace(listEl, notice('err', 'The branches could not be listed: ' + errorText(r, 'branches')));
        return;
      }
      const d = r.json || {};
      branchesListed = true;
      const rows = (d.branches || []).map((b) => h('tr', null,
        h('td', { class: 'mono' }, b.name, b.current ? h('span', null, ' ', badge('this instance', 'ok')) : null),
        h('td', { class: 'mono muted' }, b.short),
        h('td', null, b.current ? null : h('div', { class: 'row', style: { gap: '6px' } },
          perms.guard(h('button', { class: 'btn sm ghost', type: 'button', 'data-git-branch-preview': b.name, onclick: () => branch('Checking', { branch: b.name, dry_run: true, prune: prune.input.checked }) }, icon('diff'), 'Preview'), 'git.write'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', 'data-git-branch-switch': b.name, onclick: () => { if (confirm('Switch this instance to ' + b.name + ' and apply its document' + (prune.input.checked ? ' with prune' : '') + '?')) branch('Switching', { branch: b.name, prune: prune.input.checked }); } }, 'Switch'), 'git.write')))));
      replace(listEl,
        rows.length ? h('table', { class: 'table small' }, h('thead', null, h('tr', null, h('th', null, 'Branch'), h('th', null, 'Commit'), h('th', null, ''))), h('tbody', null, ...rows))
          : h('p', { class: 'muted small', style: { margin: 0 } }, 'The remote has no branch yet: the first push creates ' + d.branch + '.'),
        d.on_remote === false && rows.length ? h('p', { class: 'muted small', style: { margin: 0 } }, 'This instance is on ' + d.branch + ', which the remote does not have yet.') : null,
        d.truncated ? h('p', { class: 'muted small', style: { margin: 0 } }, 'More branches are not listed.') : null);
    }
    const listBtn = h('button', { class: 'btn sm', type: 'button', id: 'git-branches-list', disabled: !ready, onclick: list }, icon('refresh'), 'List the remote\'s branches');
    const create = (dry) => {
      const b = name.value.trim();
      if (!b) { toast('Name the new branch', 'err'); return; }
      if (dry || confirm('Start ' + b + ' from this instance? Its configuration is committed to the new branch and pushed, and the instance commits to ' + b + ' from now on.')) branch(dry ? 'Checking' : 'Starting', { branch: b, create: true, dry_run: dry });
    };
    replace(branchesEl, card('Branches', { hint: 'a branch per environment: this instance is on ' + st.branch, actions: listBtn },
      h('div', { class: 'stack' },
        listEl,
        prune.el,
        h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } },
          h('div', { style: { minWidth: '220px' } }, name),
          perms.guard(h('button', { class: 'btn sm ghost', type: 'button', id: 'git-branch-create-preview', disabled: !ready, onclick: () => create(true) }, icon('diff'), 'Preview'), 'git.write'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'git-branch-create', disabled: !ready, onclick: () => create(false) }, icon('plus'), 'Start a new branch from this instance'), 'git.write')),
        h('p', { class: 'muted small', style: { margin: 0 } }, 'The branch is the git.branch setting: a rustybin.yaml never names one, and a document from the repository cannot change it.'),
        out)));
  }

  // Commit signing: the signing key's public half and fingerprint (never
  // the private half), to register at the git host as a signing key.
  function renderSigning() {
    const sg = st.signing || {};
    const gen = perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'git-signing-generate', disabled: !st.state_dir, onclick: async () => {
      if (sg.key_set && !confirm('Generate a new signing key? Commits signed from now on verify only once the new public key is registered at the git host.')) return;
      const r = await post('/_rustybin/git/signing-key', { replace: !!sg.key_set });
      if (!r.ok) { toast(errorText(r, 'signing key'), 'err'); return; }
      toast('Signing key generated');
      refreshSettings();
      load(false);
    } }, icon('token'), sg.key_set ? 'Generate a new key' : 'Generate the signing key'), 'git.write');
    replace(signingEl, card('Commit signing', { hint: 'SSH signatures with a key of their own; ' + (sg.enabled ? 'on' : 'off (the Sign commits setting above turns it on)'), actions: gen },
      sg.public ? h('div', { class: 'stack' },
        h('div', { class: 'row', style: { gap: '6px' } }, h('code', { class: 'mono small', id: 'git-signing-public', style: { wordBreak: 'break-all' } }, sg.public), copyButton(() => sg.public, { label: 'Copy', toast: 'Public key copied' })),
        h('div', { class: 'small' }, 'Fingerprint ', h('code', { class: 'mono', id: 'git-signing-fingerprint' }, sg.fingerprint)),
        sg.problem ? notice('warn', sg.problem) : null,
        h('p', { class: 'muted small', style: { margin: 0 } }, 'Register this public key at the git host as a signing key (not as a deploy key: this key never pushes). The private half is a secret of this instance and is never shown.'))
        : h('div', { class: 'stack' },
          sg.problem ? notice('warn', sg.problem) : null,
          h('p', { class: 'muted small', style: { margin: 0 } }, 'No signing key yet. Generate one here (or paste an OpenSSH private key without a passphrase into the Signing key setting above), register its public key at the git host as a signing key, then turn Sign commits on.'))));
  }

  // Verified pulls: the switch (the git.verify setting), who may sign
  // (git.allowed_signers, public keys, edited in the form above), the head
  // trusted when it was turned on, the last verified head, the last refusal.
  function renderVerify() {
    const v = st.verification || {};
    const signers = (v.signers || []).slice();
    if (v.own_key) signers.push(Object.assign({ own: true }, v.own_key));
    const toggle = perms.guard(h('button', { class: 'btn sm', type: 'button', id: 'git-verify-toggle', disabled: !st.state_dir, onclick: async () => {
      const on = !v.enabled;
      if (on && !confirm('Verify pulled commits from now on? The head of this moment is trusted as it is; every commit after it needs an allowed signer\'s SSH signature. A squash or rebase merge made by the git host will be refused: merge with a merge commit.')) return;
      if (!on && !confirm('Stop verifying pulled commits? Whatever reaches the branch is applied again.')) return;
      const r = await send({ control: true, method: 'PATCH', url: SERVER + '/_rustybin/settings', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ values: { 'git.verify': on } }), admin: true, session: false });
      if (!r.ok) { toast(errorText(r, 'verification'), 'err'); return; }
      toast(on ? 'Pulled commits are verified' : 'Pulled commits are not verified');
      refreshSettings();
      load(false);
    } }, icon('check'), v.enabled ? 'Turn off' : 'Turn on'), 'settings.write');
    const head = (m) => h('span', null, h('code', { class: 'mono' }, m.short), h('span', { class: 'muted small' }, ' ' + fmtTime(m.at)));
    replace(verifyEl, card('Verified pulls', { hint: 'only what an allowed signer signed is taken from the repository; ' + (v.enabled ? 'on' : 'off'), actions: toggle },
      h('div', { class: 'stack' },
        v.problem ? notice('warn', v.problem) : null,
        v.enabled && v.last_refusal ? notice('err', h('div', { class: 'grow' }, h('strong', null, 'Refused: '), refusalText(v.last_refusal))) : null,
        signers.length ? h('table', { class: 'table small', id: 'git-signers' }, h('thead', null, h('tr', null, h('th', null, 'Signer'), h('th', null, 'Key'), h('th', null, 'Fingerprint'))),
          h('tbody', null, ...signers.map((s) => h('tr', null, h('td', null, s.name, s.own ? h('span', { class: 'muted small' }, ' (this instance\'s signing key)') : null), h('td', { class: 'mono' }, s.key_type), h('td', { class: 'mono' }, s.fingerprint)))))
          : h('p', { class: 'muted small', id: 'git-signers', style: { margin: 0 } }, 'No allowed signers yet. List them in the Allowed signers setting above, one a line: a name and a public key (ann ssh-ed25519 AAAA...), or the line of a .pub file.'),
        v.enabled ? kvgrid([
          ['Last verified head', v.verified_head ? h('span', { id: 'git-verified-head' }, head(v.verified_head)) : h('span', { id: 'git-verified-head' }, 'none: every commit of the branch has to verify')],
          ['Trusted when turned on', v.trusted ? head(v.trusted) : 'nothing (there was no commit yet)'],
        ]) : null,
        h('p', { class: 'muted small', style: { margin: 0 } }, 'Every commit since the last verified head needs an allowed signer\'s SSH signature: on a pull, the webhook, the automatic mode, a switch of branch, resolving with the repository\'s copy and a checkpoint from the repository. A merge commit passes unsigned only as the clean merge of two verified parents, so merge pull requests with a merge commit: a squash or rebase merge made by the git host is refused. The signers are public keys, changed in the Allowed signers setting above; this instance\'s own signing key is allowed without being listed.'))));
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
    const r = await read('/_rustybin/git/history');
    const commits = (r.ok && r.json && r.json.commits) || [];
    replace(historyEl, card('History', { hint: commits.length ? 'the last ' + commits.length + ' commits' : 'no commits yet' },
      commits.length ? h('table', { class: 'table small' }, h('thead', null, h('tr', null, h('th', null, 'Commit'), h('th', null, 'Message'), h('th', null, 'Author'), h('th', null, 'When'), h('th', null, 'Files'))),
        h('tbody', null, ...commits.map((c) => h('tr', null, h('td', { class: 'mono' }, c.short), h('td', null, c.subject), h('td', null, c.author), h('td', { class: 'muted' }, fmtTime(c.at)), h('td', { class: 'muted' }, c.stat)))))
        : h('p', { class: 'muted small', style: { margin: 0 } }, r.ok ? 'Commit the instance to start the history.' : errorText(r, 'history'))));
  }

  // The repository form (the git.* settings). A switch of branch and a new
  // signing key change settings from here, so the form is drawn again.
  const formEl = h('div', { id: 'git-settings' });
  function renderForm(d) {
    const form = settingsForm(d, 'git', { saveLabel: 'Save repository settings', onSaved: (saved) => { reload(saved); renderForm(saved); load(false); } });
    replace(formEl, form.el);
  }
  refreshSettings = async () => {
    try {
      const d = await getJson('/_rustybin/settings', { admin: true, session: false });
      reload(d);
      renderForm(d);
    } catch (e) { /* the form keeps what it showed; the status below is current */ }
  };
  if (doc) {
    renderForm(doc);
    el.appendChild(card('Repository', { hint: 'where this instance commits its configuration' }, formEl));
  } else {
    el.appendChild(card('Repository', {}, notice('err', 'Settings could not be loaded: ' + ((error && error.message) || error))));
  }
  el.appendChild(card('Status', { hint: 'the working copy against the instance and the remote', actions: h('button', { class: 'btn sm ghost', type: 'button', id: 'git-refresh', onclick: () => load(fetchNext) }, icon('refresh'), 'Refresh') }, statusEl));
  el.appendChild(actionsEl);
  el.appendChild(branchesEl);
  el.appendChild(keyEl);
  el.appendChild(signingEl);
  el.appendChild(verifyEl);
  el.appendChild(historyEl);
  el.appendChild(timelineEl);
  fetchNext = true;
  load(false);
  return el;
}
