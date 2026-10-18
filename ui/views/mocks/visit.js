// Self-service visits in the Mock APIs view: the visit bar (time left, hours
// used today, Extend), the email step and the come-back panel (the day is
// used up, the demo is busy, or the free time is over).
import { h, replace, icon, notice, toast, input, copyButton } from '../../lib/dom.js';
import { api, errorText } from '../../lib/mockapi.js';
import * as visit from '../../lib/visit.js';

const COME_BACK = ['rustybin_daily_limit', 'rustybin_visit_limit', 'rustybin_mocks_busy', 'rustybin_self_service_paused'];

/** True when a failed call is a "come back later" answer. */
export function isComeBack(r) {
  return !!(r && r.json && COME_BACK.includes(r.json.error));
}

/** The come-back panel for such an answer. */
export function comeBack(json) {
  const when = json.resets_at ? new Date(json.resets_at) : null;
  const retry = !when && json.retry_after ? new Date(Date.now() + json.retry_after * 1000) : null;
  const at = when || retry;
  return notice('warn',
    h('strong', null, json.message || 'The demo cannot take more right now.'),
    at && !Number.isNaN(at.getTime()) ? h('div', { class: 'small' }, 'You can come back from ', h('strong', null, at.toLocaleString()), '.') : null,
    h('div', { class: 'small', style: { marginTop: '6px' } },
      'Need more? ',
      h('a', { href: json.self_host_url || 'https://docs.rustybin.dev/quick-start.html', target: '_blank', rel: 'noopener' }, 'Run your own Rustybin'),
      ': one container, no time limits.'));
}

/**
 * The email step as a dialog. info: the list's `self_service` (privacy URL,
 * consent mode, hours). Resolves to {email, marketing_consent} or null.
 */
export function askEmail(info, reason) {
  return new Promise((resolve) => {
    let done = false;
    const email = input({ type: 'email', class: 'input', autocomplete: 'email', required: true, placeholder: 'you@example.com', 'aria-label': 'Email address', 'data-autofocus': true });
    const consent = h('input', { type: 'checkbox', id: 'visit-consent' });
    const err = h('div', { 'aria-live': 'polite' });
    const form = h('form', { class: 'stack' },
      h('p', { style: { margin: 0 } }, reason || `Your free ${info.anon_hours} hours are used. Give an email address to keep your mocks running, up to ${info.daily_hours} hours a day.`),
      h('label', { class: 'field' }, h('span', null, 'Email address'), email),
      info.consent === 'checkbox'
        ? h('label', { class: 'check small' }, consent, 'Send me product news about Rustybin (you can unsubscribe at any time).')
        : null,
      h('p', { class: 'tiny muted', style: { margin: 0 } },
        'We use the address to keep your demo running and, ', info.consent === 'checkbox' ? 'if you tick the box, ' : '', 'to send you news about Rustybin. ',
        info.privacy_url ? h('a', { href: info.privacy_url, target: '_blank', rel: 'noopener' }, 'Privacy notice') : null, '.'),
      err);
    const submit = h('button', { class: 'btn primary', type: 'submit' }, 'Continue');
    const dlg = h('dialog', { class: 'wizard small-dialog', 'aria-labelledby': 'visit-email-title' },
      h('div', { class: 'wizard-head row nowrap' }, h('h2', { id: 'visit-email-title', class: 'grow' }, 'Keep your mocks running'),
        h('button', { class: 'btn ghost icon', type: 'button', 'aria-label': 'Close', title: 'Close (Esc)', onclick: () => dlg.close() }, icon('x'))),
      h('div', { class: 'wizard-body' }, form),
      h('div', { class: 'wizard-foot row' }, h('span', { class: 'grow' }),
        h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'Not now'), submit));
    submit.setAttribute('form', 'visit-email-form');
    form.id = 'visit-email-form';
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      const v = email.value.trim();
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v)) {
        replace(err, notice('err', 'Enter a valid email address.'));
        return;
      }
      done = true;
      resolve({ email: v, marketing_consent: !!consent.checked });
      dlg.close();
    });
    dlg.addEventListener('close', () => { dlg.remove(); if (!done) resolve(null); });
    document.body.appendChild(dlg);
    dlg.showModal();
    email.focus();
  });
}

/**
 * Start a visit with the email step (the session used its anonymous
 * visits). Resolves to true when a token was stored.
 */
export async function startWithEmail(info, reason) {
  const answer = await askEmail(info, reason || `You have used today's free visits. Give an email address to create mocks for up to ${info.daily_hours} hours a day.`);
  if (!answer) return false;
  const r = await api('POST', '/visit', answer);
  if (r.status === 201 && r.json && r.json.manage_token) {
    visit.add(r.json.manage_token);
    return true;
  }
  toast(errorText(r, 'Starting the visit'), 'err');
  return false;
}

/**
 * The visit bar: time left (counting down), hours used today, Extend, the
 * manage link and End visit. `onChange` reloads the list.
 */
export function visitBar(info, v, onChange) {
  const left = h('strong', { class: 'mono' });
  const used = h('span', { class: 'small muted' });
  const extend = h('button', { class: 'btn sm primary', type: 'button', id: 'visit-extend' }, icon('plus'), 'Extend');
  const msg = h('div');
  const until = Date.now() + v.expires_in_secs * 1000;
  const opens = new Date(v.extendable_at);
  function tick() {
    const secs = (until - Date.now()) / 1000;
    left.textContent = visit.remaining(secs);
    const open = Date.now() >= opens.getTime();
    extend.disabled = !open || v.next_extension === 'none';
    extend.title = v.next_extension === 'none'
      ? "Today's hours are used"
      : open ? (v.next_extension === 'email' ? 'One more hour, with your email address' : 'One more hour')
        : 'Extend opens at ' + opens.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (secs <= 0) onChange();
  }
  used.textContent = visit.hours(v.minutes_used, v.minutes_allowed) + (v.email_given ? ' used today' : ' of free time');
  extend.addEventListener('click', async () => {
    extend.disabled = true;
    let r = await api('POST', '/visit/extend');
    if (r.status === 403 && r.json && r.json.error === 'rustybin_email_required') {
      const answer = await askEmail(info);
      if (!answer) { tick(); return; }
      r = await api('POST', '/visit/extend', answer);
    }
    if (r.ok && r.json) {
      toast('Your mocks now run until ' + new Date(r.json.expires_at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }));
      onChange();
      return;
    }
    if (isComeBack(r)) replace(msg, comeBack(r.json));
    else toast(errorText(r, 'Extending'), 'err');
    tick();
  });
  const token = visit.tokens()[0];
  const end = h('button', { class: 'btn sm ghost', type: 'button', title: 'Delete your mocks now and end the visit' }, 'End visit');
  end.addEventListener('click', async () => {
    if (!confirm('End your visit now? Your mocks are deleted and their URLs stop answering.')) return;
    const r = await api('DELETE', '/visit');
    if (r.ok) {
      for (const t of visit.tokens()) visit.remove(t);
      toast('Visit ended');
      onChange();
    } else {
      toast(errorText(r, 'Ending the visit'), 'err');
    }
  });
  const timer = setInterval(() => { if (!document.contains(left)) clearInterval(timer); else tick(); }, 1000);
  tick();
  return h('div', { class: 'visit-bar stack-sm', role: 'region', 'aria-label': 'Your visit' },
    h('div', { class: 'row' },
      icon('history'), h('span', null, 'Your mocks run for another ', left, '.'), used,
      h('span', { class: 'grow' }),
      token ? copyButton(() => visit.manageLink(token), { label: 'Copy manage link', toast: 'Manage link copied: open it in another browser to manage these mocks there' }) : null,
      end, extend),
    v.next_extension === 'email' ? h('div', { class: 'tiny muted' }, `After ${info.anon_hours} hours, Extend asks for an email address (up to ${info.daily_hours} hours a day).`) : null,
    msg);
}
