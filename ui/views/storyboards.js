// Demo storyboards: a demo as a script. The list, an editor (steps in
// order, each with a talk track, an action and a reset) and a presenter
// mode (the talk track large, Next runs the step's action and shows the
// verdict). GET /_rustybin/storyboards; a step runs with the signed-in
// caller's own permissions (POST .../steps/{n}/run). The same on every
// surface: rustybin storyboard, the *_storyboard tools, the storyboards:
// section of rustybin.yaml. The action forms come from the configuration
// schema (GET /_rustybin/config/schema), so a new kind of action appears
// here without a change.
import { h, replace, card, notice, icon, badge, spinner, toast, fmtTime } from '../lib/dom.js';
import { getJson, send, SERVER } from '../lib/http.js';
import { jsonView } from '../lib/format.js';
import { renderMarkdown } from '../lib/markdown.js';
import * as perms from '../lib/perms.js';
import * as projects from '../lib/projects.js';

const BASE = '/_rustybin/storyboards';
const NEW = '+new';
// The values of {{variables}} for this page's runs: typed by the presenter,
// kept in memory only.
let variablesText = '';

function parseVars(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const t = line.trim();
    const i = t.indexOf('=');
    if (i > 0) out[t.slice(0, i).trim()] = t.slice(i + 1);
  }
  return out;
}

async function call(method, path, body) {
  const opts = { control: true, admin: true, method, url: SERVER + path, timeout: 90000 };
  if (body !== undefined) {
    opts.headers = { 'Content-Type': 'application/json', 'X-Rustybin-Client': 'console' };
    opts.body = JSON.stringify(body);
  }
  const r = await send(opts);
  if (r.error) throw new Error(r.error);
  if (!r.ok) {
    const j = r.json || {};
    const e = new Error(j.error || j.message || `${r.status} ${r.statusText}`);
    e.permission = j.reason === 'permission_required' ? j.permission : null;
    throw e;
  }
  return r.json !== undefined ? r.json : r.text;
}

/** The action variants of the schema: [{ kind, description, fields: [{ name, schema, required }] }]. */
async function actionVariants() {
  try {
    const schema = await getJson('/_rustybin/config/schema', { session: false });
    const action = schema.properties.storyboards.items.properties.steps.items.properties.action;
    return action.oneOf.map((v) => ({
      kind: v.properties.type.const,
      description: v.description || '',
      fields: Object.entries(v.properties).filter(([k]) => k !== 'type')
        .map(([name, s]) => ({ name, schema: s, required: (v.required || []).includes(name) })),
    }));
  } catch {
    return null;
  }
}

const PLACEHOLDERS = {
  request: '{"method": "GET", "path": "/health", "headers": {"authorization": "Bearer {{TOKEN}}"}}',
  assert: '[{"status": 200}]',
  params: '{}',
  route: '#/requests',
  gateway: 'https://gateway.example.com (optional: absent, this instance)',
  gateway_url: 'https://gateway.example.com/route',
};

/**
 * An editor for one action. `optional`: the first choice is "nothing" (a
 * reset). Returns { el, get() -> action | undefined, throws on bad JSON }.
 */
function actionEditor(variants, value, optional, idPrefix) {
  // Without the schema: the action as JSON.
  if (!variants) {
    const raw = h('textarea', { class: 'textarea mono', rows: 3, id: idPrefix + '-json', placeholder: '{"type": "set_health", "healthy": false}' });
    raw.value = value && value.type !== 'none' ? JSON.stringify(value) : '';
    return { el: raw, get: () => (raw.value.trim() ? JSON.parse(raw.value) : undefined) };
  }
  const kinds = variants.filter((v) => !(optional && v.kind === 'none'));
  const sel = h('select', { class: 'select', id: idPrefix + '-type', 'aria-label': 'Action' });
  if (optional) sel.appendChild(h('option', { value: '' }, 'no reset'));
  for (const v of kinds) sel.appendChild(h('option', { value: v.kind, title: v.description }, v.kind === 'none' ? 'none (a talking point)' : v.kind));
  const current = value && value.type ? value.type : (optional ? '' : 'none');
  sel.value = current;
  const fieldsEl = h('div', { class: 'stack-sm' });
  const hint = h('p', { class: 'muted small' });
  let controls = [];
  const draw = (from) => {
    const v = variants.find((x) => x.kind === sel.value);
    controls = [];
    hint.textContent = v ? v.description : '';
    replace(fieldsEl, ...(v ? v.fields : []).map((f) => {
      const s = f.schema;
      const id = `${idPrefix}-${f.name}`;
      const given = from ? from[f.name] : undefined;
      let control;
      let read;
      if (Array.isArray(s.enum)) {
        control = h('select', { class: 'select', id }, ...s.enum.map((o) => h('option', { value: o }, o)));
        if (given !== undefined) control.value = given;
        read = () => control.value;
      } else if (s.type === 'boolean') {
        control = h('select', { class: 'select', id }, h('option', { value: 'true' }, 'true'), h('option', { value: 'false' }, 'false'));
        control.value = String(given === undefined ? true : !!given);
        read = () => control.value === 'true';
      } else if (s.type === 'string') {
        control = h('input', { class: 'input mono', id, type: 'text', spellcheck: 'false', autocomplete: 'off', placeholder: PLACEHOLDERS[f.name] || '' });
        control.value = given === undefined ? '' : String(given);
        read = () => (control.value.trim() === '' ? undefined : control.value.trim());
      } else {
        control = h('textarea', { class: 'textarea mono', id, rows: f.name === 'request' ? 4 : 2, spellcheck: 'false', placeholder: PLACEHOLDERS[f.name] || '' });
        control.value = given === undefined ? '' : JSON.stringify(given, null, 2);
        read = () => {
          if (control.value.trim() === '') return undefined;
          try { return JSON.parse(control.value); } catch (e) { throw new Error(`${f.name}: not JSON (${e.message})`); }
        };
      }
      controls.push({ name: f.name, required: f.required, read });
      return h('label', { class: 'field' }, h('span', null, f.name + (f.required ? '' : ' (optional)')), control,
        s.description ? h('span', { class: 'muted small' }, s.description) : null);
    }));
  };
  draw(value);
  sel.onchange = () => draw(null);
  return {
    el: h('div', { class: 'stack-sm' }, sel, hint, fieldsEl),
    get() {
      if (!sel.value) return undefined;
      const out = { type: sel.value };
      for (const c of controls) {
        const v = c.read();
        if (v === undefined) {
          if (c.required) throw new Error(`${sel.value}: ${c.name} is needed`);
          continue;
        }
        out[c.name] = v;
      }
      return out;
    },
  };
}

function describeAction(a) {
  if (!a || a.type === 'none' || !a.type) return 'A talking point';
  const rest = Object.entries(a).filter(([k]) => k !== 'type' && typeof a[k] !== 'object').map(([k, v]) => `${k} ${v}`).join(', ');
  const req = a.request ? `${a.request.method || 'GET'} ${a.request.path}` : '';
  return [a.type.replace(/_/g, ' '), req, rest].filter(Boolean).join(': ');
}

function verdictBadge(r) {
  if (!r) return badge('not run', 'outline');
  return badge(r.ok ? 'ok' : 'failed', r.ok ? 'ok' : 'err');
}

export default {
  id: 'storyboards',
  mount(root, ctx) {
    let alive = true;
    let closePresenter = null;
    const head = h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Storyboards'),
        h('p', null, 'A demo as a script: steps with what to say, what to do and how to undo it. Present it here, rehearse it with rustybin storyboard run. A step runs with your own permissions and does nothing you could not do yourself.')));
    const body = h('div', { id: 'storyboards-body' });
    root.append(head, body);

    function go(rest) {
      location.hash = '#/storyboards' + (rest ? '/' + rest : '');
    }

    // ── The list ──────────────────────────────────────────────────
    async function showList() {
      const listEl = h('div', { id: 'storyboards-list' }, spinner('Loading the storyboards'));
      const vars = h('textarea', { class: 'input mono', id: 'storyboards-vars', rows: 2, placeholder: 'NAME=value, one per line (not saved)' });
      vars.value = variablesText;
      vars.oninput = () => { variablesText = vars.value; };
      const add = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'storyboard-new', onclick: () => go(NEW) }, icon('plus'), 'New storyboard'), 'tests.write');
      replace(body, card('Storyboards', { hint: 'GET ' + BASE, actions: add },
        listEl,
        h('label', { class: 'field' }, h('span', null, 'Variables for steps that use {{NAME}}'), vars)));
      let d;
      try {
        d = await getJson(BASE + projects.query());
      } catch (e) {
        if (/^40[13] /.test(e.message)) replace(listEl, notice('', 'Storyboards need the tests.read permission: sign in with a role that grants it to see them here.'));
        else replace(listEl, notice('err', 'The storyboards could not be loaded: ' + e.message));
        return;
      }
      if (!alive) return;
      if (!d.storyboards.length) {
        replace(listEl, h('p', { class: 'muted' }, 'No storyboards yet. Write one: a checkpoint to start from, the steps of the demo, what to say at each.'));
        return;
      }
      const rows = d.storyboards.map((s) => h('tr', { 'data-storyboard': s.name },
        h('td', { class: 'mono small' }, s.name),
        h('td', null, s.title || ''),
        h('td', { class: 'small' }, String((s.steps || []).length)),
        h('td', { class: 'small' }, s.project || ''),
        h('td', { class: 'mono small' }, (s.variables || []).join(', ')),
        h('td', { class: 'small muted' }, fmtTime(s.saved_at)),
        h('td', null, h('div', { class: 'row nowrap' },
          perms.guard(h('button', { class: 'btn sm primary', type: 'button', 'data-present': s.name, onclick: () => go(encodeURIComponent(s.name) + '/present') }, icon('play'), 'Present'), 'tests.run'),
          h('button', { class: 'btn sm', type: 'button', 'data-edit': s.name, onclick: () => go(encodeURIComponent(s.name)) }, icon('edit'), 'Edit'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Reset: run every step\'s reset action', 'data-reset': s.name, onclick: () => reset(s.name).then(() => {}) }, icon('history'), 'Reset'), 'tests.run'),
          perms.guard(h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'data-delete': s.name, onclick: () => remove(s.name) }, icon('trash')), 'tests.write')))));
      replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'storyboards-table' },
        h('thead', null, h('tr', null, h('th', null, 'Name'), h('th', null, 'Title'), h('th', null, 'Steps'), h('th', null, 'Project'), h('th', null, 'Variables'), h('th', null, 'Saved'), h('th', null, ''))),
        h('tbody', null, rows))));
    }

    async function remove(name) {
      if (!window.confirm(`Delete the storyboard ${name}? What its steps changed stays as it is.`)) return;
      try {
        await call('DELETE', `${BASE}/${encodeURIComponent(name)}`);
        toast('Deleted ' + name);
        await showList();
      } catch (e) {
        toast('Could not delete: ' + e.message, 'err');
      }
    }

    /** Reset after a confirmation; resolves to the answer, or null. */
    async function reset(name) {
      if (!window.confirm(`Reset ${name}? Every step's reset action runs (a checkpoint restore deletes the API mocks it does not hold), the scenario runs it started end and your progress is cleared.`)) return null;
      try {
        const r = await call('POST', `${BASE}/${encodeURIComponent(name)}/reset`, { variables: parseVars(variablesText) });
        const failed = (r.reset || []).filter((x) => !x.ok);
        if (failed.length) toast(`Reset ${name}: ${failed.map((x) => x.summary).join('; ')}`, 'err');
        else toast(`Reset ${name}: ${(r.reset || []).length} reset action(s) ran`);
        return r;
      } catch (e) {
        toast('Could not reset: ' + (e.permission ? `it needs the ${e.permission} permission` : e.message), 'err');
        return null;
      }
    }

    // ── The editor ────────────────────────────────────────────────
    async function showEditor(name) {
      replace(body, spinner('Loading'));
      const isNew = name === NEW;
      let sb = { name: '', title: '', description: '', steps: [{ title: '', talk_track: '' }] };
      const variants = await actionVariants();
      if (!isNew) {
        try {
          sb = await getJson(`${BASE}/${encodeURIComponent(name)}`);
        } catch (e) {
          replace(body, notice('err', 'The storyboard could not be loaded: ' + e.message), h('p', null, h('a', { href: '#/storyboards' }, 'Back to the storyboards')));
          return;
        }
      }
      if (!alive) return;
      const nameIn = h('input', { class: 'input mono', id: 'storyboard-name', placeholder: 'upstream-down', maxlength: 64, value: sb.name || '', disabled: !isNew });
      const titleIn = h('input', { class: 'input', id: 'storyboard-title', placeholder: 'What the gateway does when the upstream goes down', maxlength: 200, value: sb.title || '' });
      const descIn = h('textarea', { class: 'textarea', id: 'storyboard-description', rows: 2, placeholder: 'Who it is for, what it shows (optional)' });
      descIn.value = sb.description || '';
      const stepsEl = h('div', { class: 'stack', id: 'storyboard-steps' });
      const result = h('div', { id: 'storyboard-result' });
      // Each step keeps its own editors; the order is the array's.
      let steps = [];
      const makeStep = (s) => {
        const key = Math.random().toString(36).slice(2, 8);
        const title = h('input', { class: 'input', placeholder: 'Break the upstream', maxlength: 200, value: s.title || '', 'data-step-title': '' });
        const talk = h('textarea', { class: 'textarea', rows: 3, placeholder: 'What you say here (Markdown)', 'data-step-talk': '' });
        talk.value = s.talk_track || '';
        const action = actionEditor(variants, s.action, false, 'step-' + key + '-action');
        const undo = actionEditor(variants, s.reset, true, 'step-' + key + '-reset');
        return { title, talk, action, undo };
      };
      const draw = () => {
        replace(stepsEl, ...steps.map((st, i) => h('div', { class: 'dialog-like stack-sm', 'data-step': String(i + 1) },
          h('div', { class: 'row' },
            h('strong', null, `Step ${i + 1}`),
            h('div', { class: 'grow' }),
            h('button', { class: 'btn sm', type: 'button', title: 'Move up', disabled: i === 0, 'data-step-up': '', onclick: () => { [steps[i - 1], steps[i]] = [steps[i], steps[i - 1]]; draw(); } }, icon('up')),
            h('button', { class: 'btn sm', type: 'button', title: 'Move down', disabled: i === steps.length - 1, 'data-step-down': '', onclick: () => { [steps[i + 1], steps[i]] = [steps[i], steps[i + 1]]; draw(); } }, icon('down')),
            h('button', { class: 'btn sm', type: 'button', title: 'Remove this step', 'data-step-remove': '', onclick: () => { steps.splice(i, 1); draw(); } }, icon('trash'))),
          h('label', { class: 'field' }, h('span', null, 'Title'), st.title),
          h('label', { class: 'field' }, h('span', null, 'Talk track'), st.talk),
          h('div', { class: 'row top' },
            h('div', { class: 'field grow' }, h('span', null, 'Action (what Next does)'), st.action.el),
            h('div', { class: 'field grow' }, h('span', null, 'Reset (what a reset undoes)'), st.undo.el)))));
      };
      steps = (sb.steps || []).map(makeStep);
      draw();
      const addStep = h('button', { class: 'btn', type: 'button', id: 'storyboard-add-step', onclick: () => { steps.push(makeStep({})); draw(); } }, icon('plus'), 'Add a step');
      const save = perms.guard(h('button', { class: 'btn primary', type: 'button', id: 'storyboard-save' }, icon('check'), 'Save'), 'tests.write');
      save.onclick = async () => {
        let doc;
        try {
          doc = {
            title: titleIn.value.trim() || undefined,
            description: descIn.value.trim() || undefined,
            steps: steps.map((st, i) => {
              const out = { title: st.title.value.trim() };
              if (!out.title) throw new Error(`Step ${i + 1} needs a title.`);
              if (st.talk.value.trim()) out.talk_track = st.talk.value;
              const a = st.action.get();
              if (a && a.type !== 'none') out.action = a;
              const r = st.undo.get();
              if (r) out.reset = r;
              return out;
            }),
          };
        } catch (e) {
          replace(result, notice('err', e.message));
          return;
        }
        const n = nameIn.value.trim();
        if (!n) {
          replace(result, notice('err', 'A storyboard needs a name.'));
          return;
        }
        const project = projects.forNew();
        if (isNew && project && project !== projects.INSTANCE) doc.project = project;
        try {
          if (isNew) await call('POST', BASE, Object.assign({ name: n }, doc));
          else await call('PUT', `${BASE}/${encodeURIComponent(n)}`, doc);
          toast('Saved ' + n);
          if (isNew) go(encodeURIComponent(n));
          else replace(result, notice('ok', 'Saved. Present it, or rehearse it with rustybin storyboard run ' + n + '.'));
        } catch (e) {
          replace(result, notice('err', 'Could not save: ' + e.message));
        }
      };
      const present = isNew ? null : perms.guard(h('button', { class: 'btn', type: 'button', id: 'storyboard-present', onclick: () => go(encodeURIComponent(sb.name) + '/present') }, icon('play'), 'Present'), 'tests.run');
      replace(body, card(isNew ? 'New storyboard' : sb.name, { hint: isNew ? 'POST ' + BASE : `PUT ${BASE}/${sb.name}`, actions: h('div', { class: 'row' }, h('a', { class: 'btn', href: '#/storyboards' }, icon('back'), 'All storyboards'), present, save) },
        h('div', { class: 'row' },
          h('label', { class: 'field' }, h('span', null, 'Name'), nameIn),
          h('label', { class: 'field grow' }, h('span', null, 'Title'), titleIn)),
        h('label', { class: 'field' }, h('span', null, 'Description'), descIn),
        variants ? null : notice('', 'The action forms could not be loaded: actions are edited as JSON here.'),
        stepsEl,
        h('div', { class: 'row' }, addStep),
        h('p', { class: 'muted small' }, 'A credential is never saved: a send step\'s secret-looking header holds a variable such as {{TOKEN}}, filled when the step runs.'),
        result));
    }

    // ── Presenter mode ────────────────────────────────────────────
    async function showPresenter(name) {
      replace(body, spinner('Loading'));
      let sb;
      try {
        sb = await getJson(`${BASE}/${encodeURIComponent(name)}`);
      } catch (e) {
        replace(body, notice('err', 'The storyboard could not be loaded: ' + e.message), h('p', null, h('a', { href: '#/storyboards' }, 'Back to the storyboards')));
        return;
      }
      if (!alive) return;
      replace(body, h('p', { class: 'muted' }, 'Presenting ', h('strong', null, sb.title || sb.name), '. ', h('a', { href: '#/storyboards' }, 'Leave presenter mode')));
      const steps = sb.steps || [];
      const results = new Map((sb.progress.results || []).map((r) => [r.step, r]));
      let at = Math.min(Math.max(sb.progress.current || 1, 1), Math.max(steps.length, 1));
      let busy = false;

      const sideEl = h('ol', { class: 'presenter-steps', id: 'presenter-steps' });
      const mainEl = h('div', { class: 'presenter-main', id: 'presenter-main' });
      const countEl = h('span', { class: 'muted', id: 'presenter-count' });
      const backBtn = h('button', { class: 'btn', type: 'button', id: 'presenter-back', title: 'Back one step (Left arrow)' }, icon('back'), 'Back');
      const againBtn = h('button', { class: 'btn', type: 'button', id: 'presenter-again', title: 'Run this step again (R)' }, icon('refresh'), 'Run again');
      const nextBtn = h('button', { class: 'btn primary', type: 'button', id: 'presenter-next', title: 'Right arrow or Space' });
      const overlay = h('div', { class: 'presenter', id: 'presenter', role: 'dialog', 'aria-modal': 'true', 'aria-label': 'Presenter mode: ' + (sb.title || sb.name), tabindex: '-1' },
        h('header', { class: 'presenter-head' },
          h('strong', null, sb.title || sb.name), countEl, h('div', { class: 'grow' }),
          h('button', { class: 'btn sm', type: 'button', id: 'presenter-fullscreen', onclick: () => { if (document.fullscreenElement) document.exitFullscreen(); else if (overlay.requestFullscreen) overlay.requestFullscreen().catch(() => {}); } }, 'Full screen'),
          h('button', { class: 'btn sm', type: 'button', id: 'presenter-reset', onclick: async () => { const r = await reset(sb.name); if (r) { results.clear(); at = 1; draw(); } } }, icon('history'), 'Reset'),
          h('button', { class: 'btn sm', type: 'button', id: 'presenter-exit', title: 'Leave presenter mode (Esc)', onclick: () => go('') }, icon('x'), 'Exit')),
        h('div', { class: 'presenter-body' }, h('nav', { class: 'presenter-side', 'aria-label': 'Steps' }, sideEl), mainEl),
        h('footer', { class: 'presenter-foot' }, backBtn, againBtn, h('div', { class: 'grow' }), nextBtn));

      function draw() {
        const step = steps[at - 1];
        countEl.textContent = steps.length ? `Step ${at} of ${steps.length}` : 'No steps';
        replace(sideEl, ...steps.map((s, i) => h('li', { class: i + 1 === at ? 'current' : '', 'aria-current': i + 1 === at ? 'step' : null },
          h('button', { type: 'button', class: 'presenter-step', onclick: () => { at = i + 1; draw(); } },
            verdictBadge(results.get(i + 1)), h('span', null, s.title)))));
        if (!step) {
          replace(mainEl, h('p', { class: 'muted' }, 'This storyboard has no steps yet.'));
          backBtn.disabled = againBtn.disabled = nextBtn.disabled = true;
          return;
        }
        const r = results.get(at);
        const isTalk = !step.action || step.action.type === 'none' || !step.action.type;
        const open = r && r.detail && r.detail.open;
        replace(mainEl,
          h('h2', { class: 'presenter-title' }, step.title),
          step.talk_track ? h('div', { class: 'presenter-talk' }, renderMarkdown(step.talk_track)) : null,
          h('p', { class: 'presenter-action muted' }, isTalk ? 'A talking point: nothing runs.' : (r ? 'This step ran: ' : 'Next runs: ') + describeAction(step.action)),
          r ? h('div', { class: 'presenter-verdict ' + (r.ok ? 'ok' : 'err'), id: 'presenter-verdict', role: 'status' },
            h('span', { class: 'presenter-mark', 'aria-hidden': 'true' }, r.ok ? '✓' : '✗'),
            h('div', { class: 'grow' }, h('strong', null, r.ok ? 'OK' : 'Failed'), h('div', null, r.summary),
              open ? h('p', null, h('a', { class: 'btn', href: location.pathname + open, target: 'rustybin-demo-view', rel: 'noopener' }, icon('external'), 'Open ' + open)) : null,
              r.detail && !open ? h('details', null, h('summary', null, 'What it answered'), jsonView(r.detail)) : null)) : null);
        backBtn.disabled = busy || at <= 1;
        againBtn.disabled = busy || !r || !perms.can('tests.run');
        const last = at >= steps.length;
        const label = !r ? (isTalk ? (last ? 'Finish' : 'Next') : 'Run this step') : (last ? 'Done' : 'Next step');
        replace(nextBtn, label, icon(!r && !isTalk ? 'play' : 'check'));
        nextBtn.disabled = busy || (!!r && last);
      }

      async function run() {
        const step = steps[at - 1];
        if (!step || busy) return;
        busy = true;
        draw();
        replace(nextBtn, h('span', { class: 'spinner' }), 'Running');
        try {
          const r = await call('POST', `${BASE}/${encodeURIComponent(sb.name)}/steps/${at}/run`, { variables: parseVars(variablesText) });
          results.set(at, r);
        } catch (e) {
          // A refusal is shown as the step's verdict: nothing ran.
          results.set(at, { step: at, ok: false, summary: e.permission ? `You need the ${e.permission} permission to run this step: nothing ran.` : e.message });
        }
        busy = false;
        draw();
      }

      async function next() {
        const step = steps[at - 1];
        if (!step || busy) return;
        if (!results.has(at)) {
          await run();
          // A talking point that ran moves on by itself.
          const isTalk = !step.action || step.action.type === 'none' || !step.action.type;
          if (isTalk && results.get(at) && results.get(at).ok && at < steps.length) { at += 1; draw(); }
          return;
        }
        if (at < steps.length) { at += 1; draw(); }
      }

      backBtn.onclick = () => { if (at > 1) { at -= 1; draw(); } };
      againBtn.onclick = () => run();
      nextBtn.onclick = () => next();
      const onKey = (e) => {
        if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
        const tag = (e.target && e.target.tagName) || '';
        if (e.key === 'Escape') { e.preventDefault(); go(''); return; }
        if (tag === 'BUTTON' && (e.key === ' ' || e.key === 'Enter')) return;
        if (tag === 'A' || tag === 'SUMMARY') return;
        if (e.key === 'ArrowRight' || e.key === ' ') { e.preventDefault(); next(); }
        else if (e.key === 'ArrowLeft') { e.preventDefault(); backBtn.click(); }
        else if (e.key === 'r' || e.key === 'R') { e.preventDefault(); if (!againBtn.disabled) run(); }
      };
      document.addEventListener('keydown', onKey);
      document.body.appendChild(overlay);
      const returnTo = document.activeElement;
      draw();
      overlay.focus();
      closePresenter = () => {
        document.removeEventListener('keydown', onKey);
        if (document.fullscreenElement === overlay) document.exitFullscreen().catch(() => {});
        overlay.remove();
        if (returnTo && returnTo.focus) returnTo.focus();
        closePresenter = null;
      };
    }

    function show(rest) {
      if (closePresenter) closePresenter();
      const m = /^([^/]+)(?:\/(present))?$/.exec(rest || '');
      if (!m) showList();
      else if (m[2]) showPresenter(m[1]);
      else showEditor(m[1]);
    }
    show(ctx && ctx.rest);

    return {
      onRoute: show,
      destroy() {
        alive = false;
        if (closePresenter) closePresenter();
      },
    };
  },
};
