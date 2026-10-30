// The dev portal from the console: the publish dialog (title, summary,
// tags, contact, order, demo credentials; "Write with AI" runs the
// summarise_for_portal task for a mock) and the calls behind it
// (GET /_rustybin/portal, PUT and DELETE /_rustybin/portal/items/{id}).
// Used by a mock's page and by Management, Portal.
import { h, replace, icon, notice, toast, input, textarea, field, checkbox } from './dom.js';
import { SERVER, send, getJson } from './http.js';
import * as assist from './assist.js';

const BASE = '/_rustybin/portal';

/** The portal's state: settings, every item and its publish state. */
export function load() {
  return getJson(BASE, { session: false });
}

/** The URL of the portal itself (next to the console). */
export function portalUrl() {
  return SERVER + '/portal/';
}

/** Publish (or update) item `id` with `meta`. Resolves to the send() result. */
export function publish(id, meta) {
  return send({ method: 'PUT', url: SERVER + BASE + '/items/' + encodeURIComponent(id), headers: [['Content-Type', 'application/json']], body: JSON.stringify(meta || {}), session: false, admin: true });
}

export function unpublish(id) {
  return send({ method: 'DELETE', url: SERVER + BASE + '/items/' + encodeURIComponent(id), session: false, admin: true });
}

function errorOf(r) {
  return (r.json && (r.json.error || r.json.message)) || r.error || ('HTTP ' + r.status);
}

/**
 * The publish dialog of one item. `item` is an entry of GET /_rustybin/portal
 * (id, title, summary, published); `opts.aiInputs()` returns the inputs of
 * summarise_for_portal (a mock's title, description and operations) and adds
 * "Write with AI"; `opts.onDone(view)` runs after a publish or unpublish.
 */
export function openPublishDialog(item, opts = {}) {
  const p = item.published || {};
  const id = 'portal-dlg-title';
  const title = input({ value: p.title || '', placeholder: item.title, 'aria-label': 'Portal title', maxlength: 120, id: 'portal-title' });
  const summary = textarea({ rows: 5, 'aria-label': 'Portal summary', placeholder: item.summary || 'What testers can do with it (Markdown)', id: 'portal-summary', class: 'textarea' });
  summary.value = p.summary || '';
  const tags = input({ value: (p.tags || []).join(', '), placeholder: 'payments, demo', 'aria-label': 'Portal tags', id: 'portal-tags' });
  const contact = input({ value: p.contact || '', placeholder: 'team@example.com or a URL', 'aria-label': 'Portal contact', maxlength: 200 });
  const order = input({ type: 'number', value: String(p.order || 0), min: -1000, max: 1000, step: 1, 'aria-label': 'Portal order', class: 'input mono', style: { width: '110px' } });
  const creds = checkbox('Show the demo credentials the mock accepts', p.credentials !== false);
  const err = h('div');
  const aiOut = h('div');
  const body = h('div', { class: 'wizard-body stack' });
  const foot = h('div', { class: 'wizard-foot row' });
  const dlg = h('dialog', { class: 'wizard small-dialog', 'aria-labelledby': id },
    h('div', { class: 'wizard-head row nowrap' }, h('h2', { id, class: 'grow' }, (p.published_at ? 'Portal entry of ' : 'Publish ') + item.title),
      h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', title: 'Close (Esc)', onclick: () => dlg.close() }, icon('x'))),
    body, foot);
  dlg.addEventListener('close', () => dlg.remove());

  let aiBtn = null;
  if (opts.aiInputs) {
    aiBtn = h('button', { class: 'btn sm', type: 'button', id: 'portal-write-ai', 'data-assist': 'summarise_for_portal' }, icon('ai'), 'Write with AI');
    aiBtn.addEventListener('click', async () => {
      if (!assist.enabled()) { dlg.close(); location.hash = '#/manage/ai'; toast('Add an AI key under Management, AI'); return; }
      aiBtn.disabled = true;
      replace(aiOut, h('span', { class: 'muted small' }, 'Writing...'));
      const r = await assist.run('summarise_for_portal', opts.aiInputs(), {});
      aiBtn.disabled = false;
      if (r.error || !r.json) { replace(aiOut, notice('err', 'No summary came back: ' + (r.error || 'empty answer'))); return; }
      title.value = r.json.title || title.value;
      summary.value = r.json.summary || summary.value;
      if (Array.isArray(r.json.tags) && r.json.tags.length) tags.value = r.json.tags.join(', ');
      replace(aiOut, notice('ok', 'Written by ' + (r.usage && r.usage.model ? r.usage.model : 'the assistant') + '. Check it, then publish.'));
    });
  }

  replace(body,
    item.kind === 'api' && item.state !== 'live' ? notice('warn', 'This mock is ' + item.state + ': the portal shows it once it is deployed' + (item.state === 'off' ? ' (it answers 503 while off)' : '') + '.') : null,
    field('Title', title, 'Empty: the item\'s own title (' + item.title + ').'),
    field('Summary', summary, 'Markdown. Empty: the item\'s own description.'),
    aiBtn ? h('div', { class: 'row' }, aiBtn, h('div', { class: 'grow' }, aiOut)) : null,
    h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '12px' } },
      h('div', { class: 'grow' }, field('Tags', tags, 'Comma separated, at most 10.')),
      field('Order', order, 'Lower comes first.')),
    field('Contact', contact, 'Who testers ask about it (optional).'),
    item.kind === 'api' ? creds.el : null,
    err);
  const publishBtn = h('button', { class: 'btn primary', type: 'button', id: 'portal-publish' }, icon('upload'), p.published_at ? 'Save' : 'Publish');
  const unpublishBtn = p.published_at && p.source !== 'file' ? h('button', { class: 'btn danger', type: 'button', id: 'portal-unpublish' }, 'Unpublish') : null;
  publishBtn.addEventListener('click', async () => {
    const meta = {
      title: title.value.trim(),
      summary: summary.value.trim(),
      tags: tags.value.split(',').map((t) => t.trim()).filter(Boolean),
      contact: contact.value.trim(),
      order: Number(order.value) || 0,
    };
    if (item.kind === 'api') meta.credentials = creds.input.checked;
    publishBtn.disabled = true;
    const r = await publish(item.id, meta);
    publishBtn.disabled = false;
    if (!r.ok) { replace(err, notice('err', errorOf(r))); return; }
    dlg.close();
    toast((r.status === 201 ? 'Published ' : 'Updated ') + item.title + (r.json && r.json.hidden ? ' (hidden: ' + r.json.hidden + ')' : ''));
    if (opts.onDone) opts.onDone(r.json);
  });
  if (unpublishBtn) {
    unpublishBtn.addEventListener('click', async () => {
      const r = await unpublish(item.id);
      if (!r.ok) { replace(err, notice('err', errorOf(r))); return; }
      dlg.close();
      toast('Unpublished ' + item.title);
      if (opts.onDone) opts.onDone(null);
    });
  }
  replace(foot, unpublishBtn, h('span', { class: 'grow' }),
    p.source === 'file' ? h('span', { class: 'muted small' }, 'Published by the config file: change it there.') : null,
    h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Cancel'),
    p.source === 'file' ? null : publishBtn);
  document.body.appendChild(dlg);
  dlg.showModal();
  title.focus();
}
