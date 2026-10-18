// Create wizard of the Mock APIs view: a modal with three steps (spec,
// review, behaviour). The spec is parsed as soon as it arrives
// (POST /_rustybin/mocks/import); creating runs the deploy checks and offers
// "Create as draft" or "Deploy anyway" when they fail.
import { h, replace, clear, icon, notice, spinner, methodBadge, badge, field, input, select, textarea, toggleSwitch } from '../../lib/dom.js';
import { api, errorText, nameProblem, mockUrl, readFile, groupByTag, slug } from '../../lib/mockapi.js';
import * as visit from '../../lib/visit.js';
import { comeBack, isComeBack, startWithEmail } from './visit.js';

const STEPS = ['Spec', 'Review', 'Behaviour'];
const MODES = [
  ['examples', 'Use spec examples', 'Answer with the examples of the spec, and realistic data generated from the schema where there are none.', true],
  ['generated', 'Generate realistic data', 'Always answer with data generated from the response schemas (same seed, same data).'],
  ['stateful', 'Remember changes (stateful CRUD)', 'POST stores an item, GET reads it back, PUT and PATCH update it, DELETE removes it. Collections start from the list example.'],
  ['static', "I'll define responses", 'Answer with empty bodies until you set custom responses or rules per operation.'],
];

/** Text of the built-in sample spec, exactly as uploaded. */
export async function sampleSpecText() {
  const r = await api('GET', '/petstore/spec');
  if (!r.ok || !r.text) throw new Error(r.error || 'The built-in Petstore sample is not available on this instance (RUSTYBIN_MOCKS_SAMPLE is off or it was deleted).');
  return r.text;
}

/**
 * Open the wizard. opts: {taken: [names], text?: spec text to parse at once,
 * maxBytes?, visitor?: the list's self_service info (a self-service visitor:
 * names get a random suffix, no stateful mode), onCreated(detail, deployed)}.
 */
export function openWizard(opts = {}) {
  const st = { step: 0, text: opts.text || '', preview: null, name: '', strip: false, mode: 'examples', secure: false, validate: 'warn', busy: false };
  const dlg = h('dialog', { class: 'wizard', 'aria-labelledby': 'wizard-title' });
  const stepsEl = h('ol', { class: 'wizard-steps' });
  const body = h('div', { class: 'wizard-body' });
  const foot = h('div', { class: 'wizard-foot row' });
  dlg.append(
    h('div', { class: 'wizard-head row nowrap' }, h('h2', { id: 'wizard-title', class: 'grow' }, 'New mock API'),
      h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', title: 'Close (Esc)', onclick: () => dlg.close() }, icon('x'))),
    stepsEl, body, foot);
  dlg.addEventListener('close', () => dlg.remove());
  document.body.appendChild(dlg);
  dlg.showModal();

  function renderSteps() {
    clear(stepsEl);
    STEPS.forEach((label, i) => stepsEl.appendChild(h('li', { class: i < st.step ? 'done' : '', 'aria-current': i === st.step ? 'step' : null },
      h('span', { class: 'step-n' }, i < st.step ? icon('check') : String(i + 1)), h('span', null, label))));
  }

  function go(step) {
    st.step = step;
    renderSteps();
    [stepSpec, stepReview, stepBehaviour][step]();
    const first = body.querySelector('[data-autofocus]') || body.querySelector('input, textarea, select, button');
    if (first) first.focus();
  }

  // ── Step 1: the spec ──
  const specErr = h('div');
  const paste = textarea({ rows: 10, placeholder: 'openapi: 3.0.3\ninfo:\n  title: Orders\n  version: "1.0"\npaths: ...', 'aria-label': 'OpenAPI document (JSON or YAML)' });
  paste.addEventListener('paste', () => setTimeout(() => { if (paste.value.trim()) parse(paste.value); }, 0));
  const fileIn = h('input', { type: 'file', accept: '.json,.yaml,.yml,application/json,application/yaml,text/yaml', hidden: true });
  fileIn.addEventListener('change', () => { if (fileIn.files[0]) loadFile(fileIn.files[0]); fileIn.value = ''; });

  async function loadFile(file) {
    try {
      const text = await readFile(file, opts.maxBytes);
      paste.value = text;
      parse(text);
    } catch (e) {
      replace(specErr, notice('err', e.message));
    }
  }

  async function parse(text) {
    if (st.busy) return;
    st.text = text;
    st.busy = true;
    replace(specErr, spinner('Reading the spec'));
    const r = await api('POST', '/import', { spec: text });
    st.busy = false;
    if (!dlg.open) return;
    if (!r.ok || !r.json) {
      const msg = errorText(r, 'Parsing the spec');
      const line = /line (\d+)/i.exec(msg);
      replace(specErr, notice('err', h('strong', null, 'This is not a usable OpenAPI document. '), msg,
        line ? h('div', { style: { marginTop: '6px' } }, h('button', { class: 'btn sm', type: 'button', onclick: () => showLine(Number(line[1])) }, 'Show line ' + line[1])) : null));
      return;
    }
    st.preview = r.json;
    st.name = r.json.suggested_name;
    st.secure = !!r.json.declares_security;
    st.strip = false;
    go(1);
  }

  function showLine(n) {
    const lines = paste.value.split('\n');
    let start = 0;
    for (let i = 0; i < n - 1 && i < lines.length; i++) start += lines[i].length + 1;
    const end = start + (lines[n - 1] || '').length;
    paste.focus();
    paste.setSelectionRange(start, end);
    paste.scrollTop = Math.max(0, (n - 4) * 19);
  }

  function stepSpec() {
    const drop = h('div', { class: 'dropzone', 'data-autofocus': true, tabindex: '0', role: 'button', 'aria-label': 'Choose an OpenAPI file' },
      icon('upload'), h('strong', null, 'Drop an OpenAPI file here'), h('span', { class: 'muted small' }, 'JSON or YAML: OpenAPI 3.0 / 3.1 or Swagger 2.0'),
      h('span', { class: 'btn sm' }, icon('file'), 'Browse files'));
    drop.addEventListener('click', () => fileIn.click());
    drop.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); fileIn.click(); } });
    wireDrop(drop, loadFile);
    replace(body, h('div', { class: 'stack' }, drop, fileIn,
      h('div', { class: 'field' }, h('span', null, 'Or paste it'), paste, h('span', { class: 'hint' }, 'Pasting parses it right away.')),
      specErr,
      h('p', { class: 'small muted' }, 'No spec at hand? ', h('button', { class: 'linklike', type: 'button', onclick: useSample }, 'Use the Petstore sample'), '.')));
    replace(foot, h('span', { class: 'grow' }),
      h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'),
      h('button', { class: 'btn primary', type: 'button', onclick: () => { if (paste.value.trim()) parse(paste.value); else replace(specErr, notice('warn', 'Drop a file, browse for one or paste a spec first.')); } }, 'Continue'));
  }

  async function useSample() {
    replace(specErr, spinner('Loading the sample'));
    try {
      const text = await sampleSpecText();
      paste.value = text;
      st.busy = false;
      parse(text);
    } catch (e) {
      replace(specErr, notice('err', e.message));
    }
  }

  // ── Step 2: review ──
  function stepReview() {
    const p = st.preview;
    const nameIn = input({ value: st.name, class: 'input mono', 'aria-describedby': 'wizard-name-hint', 'data-autofocus': true, maxlength: '40', 'aria-label': 'Mock name' });
    const urlEl = h('code', { class: 'break' });
    const nameMsg = h('span', { class: 'hint', id: 'wizard-name-hint', 'aria-live': 'polite' });
    const nextBtn = h('button', { class: 'btn primary', type: 'button', onclick: () => { if (!problem()) go(2); } }, 'Next');
    const strip = p.server_prefix ? h('input', { type: 'checkbox', checked: st.strip }) : null;
    function problem() {
      st.name = nameIn.value.trim();
      const v = opts.visitor;
      const why = v
        ? (!st.name ? 'Pick a name.' : st.name.length > 33 ? 'At most 33 characters (a random suffix is added).' : '')
        : nameProblem(st.name, opts.taken || []);
      const shown = v ? slug(st.name || 'name').slice(0, 33) + '-xxxxxx' : st.name || '<name>';
      urlEl.textContent = v && v.host
        ? 'https://' + shown + '.' + v.host + (st.strip ? '' : p.server_prefix || '')
        : mockUrl(shown, st.strip ? '' : p.server_prefix);
      nameMsg.textContent = why || (v ? 'A random suffix (xxxxxx) keeps your mock\'s address private.' : 'Available.');
      nameMsg.classList.toggle('err-text', !!why);
      nameIn.setAttribute('aria-invalid', String(!!why));
      nextBtn.disabled = !!why;
      return why;
    }
    nameIn.addEventListener('input', () => {
      const lower = nameIn.value.toLowerCase().replace(/[\s_]+/g, '-');
      if (lower !== nameIn.value) nameIn.value = lower;
      problem();
    });
    nameIn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); if (!problem()) go(2); } });
    if (strip) strip.addEventListener('change', () => { st.strip = strip.checked; problem(); });
    problem();
    const groups = groupByTag(p.operations || []);
    const opsEl = h('div', { class: 'wizard-ops' });
    for (const [tag, ops] of groups) {
      opsEl.appendChild(h('div', { class: 'nav-section', style: { padding: '8px 0 2px' } }, tag));
      for (const op of ops) {
        opsEl.appendChild(h('div', { class: 'op-line' }, methodBadge(op.method), h('span', { class: 'mono ellipsis' }, op.path),
          h('span', { class: 'muted small ellipsis grow' }, op.summary || op.id), op.spec_security ? h('span', { class: 'lock', title: 'Declares security' }, icon('lock')) : null,
          op.deprecated ? badge('deprecated', 'warn') : null));
      }
    }
    const warnings = p.warnings || [];
    replace(body, h('div', { class: 'stack' },
      h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('h3', { style: { fontSize: '18px' } }, p.title || 'Untitled API'), badge('v' + (p.version || '?'), 'outline'), badge((String(p.openapi || '').startsWith('2') ? 'Swagger ' : 'OpenAPI ') + p.openapi, 'info')),
        p.description ? h('p', { class: 'small muted clamp-3' }, p.description) : null),
      h('div', { class: 'field' }, h('span', null, 'Name'), nameIn, nameMsg,
        h('div', { class: 'small' }, h('span', { class: 'muted' }, 'Base URL: '), urlEl),
        strip ? h('label', { class: 'check small' }, strip, 'Serve the paths without the spec\'s ', h('code', null, p.server_prefix), ' prefix') : null),
      h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, `${(p.operations || []).length} operations`), opsEl),
      (p.security_schemes || []).length ? h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Security schemes'),
        h('div', { class: 'pill-list' }, ...p.security_schemes.map((s) => badge(h('span', null, icon('lock'), ' ', s.name, ' (', s.type.replace('_', ' '), ')'), 'violet')))) : null,
      warnings.length ? h('details', { class: 'disclosure' }, h('summary', null, `${warnings.length} warning${warnings.length === 1 ? '' : 's'} about the spec`),
        h('ul', { class: 'small issue-list' }, ...warnings.map((w) => h('li', null, w)))) : null,
      p.report && !p.report.ok ? notice('warn', `The deploy checks found ${p.report.errors} problem${p.report.errors === 1 ? '' : 's'} with the default settings. You can still create it (as a draft, or deploy anyway) and fix them in the editor.`) : null));
    replace(foot, h('button', { class: 'btn', type: 'button', onclick: () => go(0) }, 'Back'), h('span', { class: 'grow' }), nextBtn);
  }

  // ── Step 3: behaviour ──
  function stepBehaviour() {
    const p = st.preview;
    const result = h('div');
    const cards = h('div', { class: 'radio-cards', role: 'radiogroup', 'aria-label': 'Responses' });
    for (const [value, label, text, recommended] of MODES.filter(([value]) => !opts.visitor || value !== 'stateful')) {
      const radio = h('input', { type: 'radio', name: 'wizard-mode', value, checked: st.mode === value, 'data-autofocus': st.mode === value ? true : null });
      radio.addEventListener('change', () => { st.mode = value; });
      cards.appendChild(h('label', { class: 'radio-card' }, radio,
        h('span', { class: 'stack-sm', style: { gap: '2px' } }, h('strong', null, label, recommended ? h('span', { class: 'badge ok', style: { marginLeft: '8px' } }, 'recommended') : null),
          h('span', { class: 'small muted' }, text))));
    }
    const secure = toggleSwitch('Secure this mock', st.secure, (v) => { st.secure = v; });
    const validate = select([['off', 'Off: accept anything'], ['warn', 'Warn: answer, flag problems'], ['enforce', 'Enforce: answer 400']], st.validate, { 'aria-label': 'Validate requests' });
    validate.addEventListener('change', () => { st.validate = validate.value; });
    const schemes = (p.security_schemes || []).map((s) => s.name).join(', ');
    replace(body, h('div', { class: 'stack' },
      h('fieldset', { class: 'plain stack-sm' }, h('legend', { class: 'label' }, 'Responses'), cards),
      h('div', { class: 'row nowrap', style: { alignItems: 'flex-start' } }, secure.el,
        h('div', { class: 'grow' }, h('strong', null, 'Secure this mock'),
          h('div', { class: 'small muted' }, p.declares_security
            ? `Calls need the credentials of the spec's schemes (${schemes}). Demo credentials are shown after creation.`
            : 'The spec declares no security: every call will need the header x-api-key: demo-key.'))),
      field('Validate requests', validate, 'Checks parameters and bodies against the spec. Warn answers normally and flags the problems in the traffic view.'),
      result));
    const draftBtn = h('button', { class: 'btn', type: 'button', onclick: () => create(false, false) }, 'Create as draft');
    const deployBtn = h('button', { class: 'btn primary', type: 'button', onclick: () => create(true, false) }, icon('up'), 'Create and deploy');
    replace(foot, h('button', { class: 'btn', type: 'button', onclick: () => go(1) }, 'Back'), h('span', { class: 'grow' }), draftBtn, deployBtn);

    async function create(deploy, force) {
      draftBtn.disabled = deployBtn.disabled = true;
      replace(result, spinner(deploy ? 'Creating and running the deploy checks' : 'Creating the draft'));
      const payload = {
        spec: st.text,
        name: st.name,
        deploy,
        force,
        strip_server_prefix: st.strip,
        responses: { mode: st.mode, seed: 1, delay_ms: 0, validate_requests: st.validate, disabled_status: 503 },
        security: { enabled: st.secure, mode: p.declares_security ? 'spec' : 'api_key' },
      };
      const r = await api('POST', '', payload, { timeout: 60000 });
      draftBtn.disabled = deployBtn.disabled = false;
      if (!dlg.open) return;
      if (r.status === 201 && r.json) {
        if (r.json.manage_token) visit.add(r.json.manage_token);
        dlg.close();
        if (opts.onCreated) opts.onCreated(r.json, deploy);
        return;
      }
      if (opts.visitor && r.status === 403 && r.json && r.json.error === 'rustybin_email_required') {
        replace(result);
        if (await startWithEmail(opts.visitor)) create(deploy, force);
        return;
      }
      if (isComeBack(r)) {
        replace(result, comeBack(r.json));
        return;
      }
      if (r.status === 422 && r.json && r.json.report) {
        replace(result, checksFailed(r.json.report, () => create(true, true), () => create(false, false)));
        return;
      }
      if (r.status === 409 && r.json && r.json.error === 'rustybin_mock_cap') {
        replace(result, notice('err', errorText(r), ' ', h('a', { href: '#/mocks', onclick: () => dlg.close() }, 'Your mocks')));
        return;
      }
      if (r.status === 409) {
        replace(result, notice('err', errorText(r), ' ', h('button', { class: 'btn sm', type: 'button', onclick: () => go(1) }, 'Change the name')));
        return;
      }
      replace(result, notice('err', errorText(r, 'Creating the mock')));
    }
  }

  renderSteps();
  if (st.text) {
    stepSpec();
    paste.value = st.text;
    parse(st.text);
  } else {
    go(0);
  }
  return dlg;
}

/** The failing deploy checks with "Deploy anyway" and "Create as draft". */
export function checksFailed(report, onForce, onDraft) {
  const bad = (report.operations || []).filter((o) => o.status === 'error');
  const items = [];
  for (const f of report.findings || []) if (f.level === 'error') items.push(h('li', null, f.message));
  for (const o of bad.slice(0, 8)) {
    for (const f of o.findings.filter((x) => x.level === 'error')) items.push(h('li', null, methodBadge(o.method), ' ', h('span', { class: 'mono' }, o.path), ': ', f.message));
  }
  if (bad.length > 8) items.push(h('li', { class: 'muted' }, `and ${bad.length - 8} more operations`));
  return notice('err', h('strong', null, `The deploy checks found ${report.errors} error${report.errors === 1 ? '' : 's'}.`),
    h('ul', { class: 'issue-list small' }, ...items),
    h('div', { class: 'row', style: { marginTop: '8px' } },
      onDraft ? h('button', { class: 'btn sm', type: 'button', onclick: onDraft }, 'Create as draft') : null,
      onForce ? h('button', { class: 'btn sm danger', type: 'button', onclick: onForce }, 'Deploy anyway') : null));
}

/** Make an element accept dropped files (with a highlight while dragging). */
export function wireDrop(el, onFile) {
  el.addEventListener('dragover', (e) => { e.preventDefault(); el.classList.add('drag'); });
  el.addEventListener('dragleave', () => el.classList.remove('drag'));
  el.addEventListener('drop', (e) => {
    e.preventDefault();
    el.classList.remove('drag');
    const f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    if (f) onFile(f);
  });
}
