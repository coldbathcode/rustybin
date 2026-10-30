// Management, Export: the whole instance as rustybin.yaml (GET
// /_rustybin/config/export: copy, download, the bundle), the diff against
// the last export seen here, and "Apply a document" (POST
// /_rustybin/config/apply: paste or upload, a dry run with the diff first,
// then apply; prune is an explicit checkbox).
import { h, replace, card, field, textarea, checkbox, notice, toast, icon, spinner, copyButton, select } from '../../lib/dom.js';
import { SERVER, send } from '../../lib/http.js';
import { download, readFile, errorText } from '../../lib/mockapi.js';

const LAST_EXPORT_KEY = 'rustybin.config.lastExport';
const MAX_DOCUMENT = 8 * 1024 * 1024;

function changeList(report) {
  const summary = (report && report.summary) || [];
  const problems = (report && report.problems) || [];
  const out = h('div', { class: 'stack', id: 'config-changes' });
  if (!summary.length) out.appendChild(notice('ok', icon('check'), h('div', { class: 'grow' }, 'No changes: the instance already matches the document.')));
  else {
    out.appendChild(h('p', { class: 'small', style: { margin: 0 } }, summary.length + ' change' + (summary.length === 1 ? '' : 's') + (report.dry_run ? ' would be applied' : ' applied') + (report.prune ? ' (with prune)' : '') + ':'));
    out.appendChild(h('ul', { class: 'small mono', style: { margin: 0, paddingLeft: '18px' } }, ...summary.map((l) => h('li', null, l))));
  }
  if (problems.length) out.appendChild(notice('warn', h('div', { class: 'grow' }, h('strong', null, 'Problems: '), h('ul', { style: { margin: '4px 0 0', paddingLeft: '18px' } }, ...problems.map((p) => h('li', null, p))))));
  return out;
}

export function exportPanel() {
  const el = h('div', { class: 'stack' });
  const secrets = select([['refs', 'secrets as references (${VAR}, secret:stored)'], ['omit', 'no secrets section, demo credentials redacted']], 'refs', { id: 'config-secrets', 'aria-label': 'Secrets in the export' });
  const yamlEl = textarea({ id: 'config-yaml', readonly: true, rows: 18, 'aria-label': 'The instance as rustybin.yaml', style: { width: '100%' } });
  const status = h('div');
  const diffEl = h('div');
  let current = '';

  async function load() {
    replace(status, spinner('Exporting'));
    const r = await send({ url: SERVER + '/_rustybin/config/export?secrets=' + secrets.value, admin: true, session: false });
    if (!r.ok) { replace(status, notice('err', errorText(r, 'export'))); return; }
    current = r.text;
    yamlEl.value = current;
    const last = sessionStorage.getItem(LAST_EXPORT_KEY);
    replace(status, h('span', { class: 'muted small' }, 'Exported ' + new Date().toLocaleTimeString() + ', ' + current.length + ' characters.'));
    if (last && last !== current) {
      // The diff against the last export seen here: the server names what changed.
      const d = await send({ method: 'POST', url: SERVER + '/_rustybin/config/apply', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ document: last, dry_run: true }), admin: true, session: false });
      if (d.ok && d.json) {
        const lines = d.json.summary || [];
        replace(diffEl, card('Since the last export in this browser', { hint: lines.length ? lines.length + ' difference(s), read as "what applying the old export would change"' : 'nothing changed' },
          lines.length ? h('ul', { class: 'small mono', style: { margin: 0, paddingLeft: '18px' } }, ...lines.map((l) => h('li', null, l))) : h('p', { class: 'muted small', style: { margin: 0 } }, 'The instance is as it was.')));
      }
    } else replace(diffEl);
    sessionStorage.setItem(LAST_EXPORT_KEY, current);
  }

  async function bundle() {
    const r = await send({ url: SERVER + '/_rustybin/config/export?format=bundle&secrets=' + secrets.value, admin: true, session: false, stream: true });
    if (!r.ok || !r.response) { toast(errorText(r, 'bundle'), 'err'); return; }
    const blob = await r.response.blob();
    const url = URL.createObjectURL(blob);
    const a = h('a', { href: url, download: 'rustybin-config.tar.gz', style: { display: 'none' } });
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  secrets.addEventListener('change', load);
  el.appendChild(card('This instance as rustybin.yaml', { hint: 'version 2: settings, mocks, registry, portal, scenarios, agents, health', actions: h('div', { class: 'row', style: { gap: '6px' } },
    copyButton(() => current, { label: 'Copy', toast: 'YAML copied' }),
    h('button', { class: 'btn sm', type: 'button', id: 'config-download', onclick: () => download('rustybin.yaml', current, 'application/yaml') }, icon('download'), 'Download'),
    h('button', { class: 'btn sm', type: 'button', id: 'config-bundle', title: 'rustybin.yaml plus specs/ as a .tar.gz', onclick: bundle }, icon('download'), 'Bundle'),
    h('button', { class: 'btn sm ghost', type: 'button', id: 'config-refresh', onclick: load }, icon('refresh'), 'Refresh')) },
  h('div', { class: 'stack' },
    field('Secrets', secrets, 'Secrets are never exported as values: a reference says which variable or that the instance holds one. Commit the references, keep the values in the environment.'),
    yamlEl, status)));
  el.appendChild(diffEl);
  el.appendChild(applyCard());
  load();
  return el;
}

function applyCard() {
  const doc = textarea({ id: 'config-document', rows: 10, placeholder: 'version: 2\nhealth: { initial: healthy }\nmocks:\n  - name: orders\n    spec_inline: { openapi: 3.0.3, ... }', 'aria-label': 'Document to apply', style: { width: '100%' } });
  const file = h('input', { type: 'file', accept: '.yaml,.yml,.json,text/yaml,application/json', id: 'config-file', 'aria-label': 'Upload a document' });
  const prune = checkbox('Prune: a present section is the whole truth (API mocks, published items and entries, saved scenarios and stored settings it does not name are removed)', false);
  prune.input.id = 'config-prune';
  const result = h('div', { id: 'config-result' });
  const applyBtn = h('button', { class: 'btn primary', type: 'button', id: 'config-apply', disabled: true }, 'Apply');
  const dryBtn = h('button', { class: 'btn', type: 'button', id: 'config-dry-run' }, icon('play'), 'Dry run');
  let checked = null;

  file.addEventListener('change', async () => {
    const f = file.files && file.files[0];
    if (!f) return;
    try { doc.value = await readFile(f, MAX_DOCUMENT); checked = null; applyBtn.disabled = true; } catch (e) { toast(e.message, 'err'); }
  });
  doc.addEventListener('input', () => { checked = null; applyBtn.disabled = true; });

  async function run(dryRun) {
    const text = doc.value;
    if (!text.trim()) { replace(result, notice('warn', 'Paste or upload a document first.')); return; }
    replace(result, spinner(dryRun ? 'Checking' : 'Applying'));
    const r = await send({ method: 'POST', url: SERVER + '/_rustybin/config/apply', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ document: text, prune: prune.input.checked, dry_run: dryRun }), admin: true, session: false, timeout: 120000 });
    if (!r.ok || !r.json) { replace(result, notice('err', errorText(r, 'apply'))); return; }
    const report = r.json;
    const head = dryRun
      ? notice('', icon('info'), h('div', { class: 'grow' }, h('strong', null, 'Dry run. '), 'Nothing changed. Sections: ' + (report.sections || []).join(', ') + '.'))
      : notice(report.ok ? 'ok' : 'warn', icon(report.ok ? 'check' : 'alert'), h('div', { class: 'grow' }, h('strong', null, report.ok ? 'Applied. ' : 'Applied with problems. '), 'Sections: ' + (report.sections || []).join(', ') + '.'));
    replace(result, head, changeList(report));
    if (dryRun) { checked = text; applyBtn.disabled = false; applyBtn.textContent = (report.summary || []).length ? 'Apply ' + report.summary.length + ' change' + (report.summary.length === 1 ? '' : 's') : 'Apply (no changes)'; }
    else { checked = null; applyBtn.disabled = true; applyBtn.textContent = 'Apply'; toast(report.ok ? 'Document applied' : 'Applied with problems', report.ok ? '' : 'err'); }
  }
  dryBtn.addEventListener('click', () => run(true));
  applyBtn.addEventListener('click', () => {
    if (checked !== doc.value) { toast('Run the dry run first'); return; }
    if (prune.input.checked && !confirm('Apply with prune? Everything a present section does not name is removed from this instance.')) return;
    run(false);
  });
  return card('Apply a document', { hint: 'a dry run shows what would change, then Apply' }, h('div', { class: 'stack' },
    field('Document', doc, 'YAML or JSON, version 1 (mocks only) or 2. Sections the document leaves out are left alone. Spec file paths are not readable here: inline the specs (spec_inline) or pull from a repository under Source control.'),
    h('div', { class: 'row', style: { gap: '8px', flexWrap: 'wrap' } }, file, h('span', { class: 'grow' }), prune.el),
    h('div', { class: 'row', style: { gap: '8px' } }, dryBtn, applyBtn, h('span', { class: 'muted small' }, 'Apply is enabled by a dry run of the same text.')),
    result));
}
