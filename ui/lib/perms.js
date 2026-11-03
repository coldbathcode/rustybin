// What the signed-in caller may do: the permissions GET /_rustybin/session
// answers (the console asks, never guesses). A control the caller cannot use
// is disabled with a title naming the permission it needs; the server checks
// every request anyway (a 403 says reason permission_required).
//
// Unknown (an open control plane, no session route, not signed in yet):
// every control stays as it was, and the server's answer decides.

import { send, SERVER } from './http.js';
import * as auth from './auth.js';
import * as settings from './settings.js';

let known = null; // array of permission names, or null (not known)
let roles = [];

/** Load the caller's permissions; resolves true when they changed. */
export async function load() {
  const before = JSON.stringify(known);
  if (!auth.required() || auth.needsSignIn()) {
    known = null;
    roles = [];
  } else {
    const r = await send({ control: true, url: SERVER + '/_rustybin/session', session: false, timeout: 15000 });
    if (r.ok && r.json && Array.isArray(r.json.permissions)) {
      known = r.json.permissions.map(String);
      roles = Array.isArray(r.json.roles) ? r.json.roles.map(String) : [];
    } else if (r.status === 401 || r.status === 404) {
      known = null;
      roles = [];
    }
  }
  return JSON.stringify(known) !== before;
}

/** Forget them (sign-out, another user). */
export function reset() {
  known = null;
  roles = [];
}

/** True when the console knows the caller's permissions. */
export function isKnown() {
  return known !== null;
}

/** The caller's roles, when known. */
export function roleNames() {
  return roles.slice();
}

/** May the caller do what `p` allows? (An admin token in the console's settings is every permission.) */
export function can(p) {
  if (settings.get('adminToken')) return true;
  return known === null || known.includes(p);
}

/** May the caller do all of `list`? */
export function canAll(list) {
  return (list || []).every(can);
}

/** The first permission of `list` the caller lacks, or ''. */
export function missing(list) {
  return (list || []).find((p) => !can(p)) || '';
}

/** Why a control is off: '' when the caller holds `p`. */
export function reason(p) {
  return can(p) ? '' : 'Needs the ' + p + ' permission, which your roles do not grant.';
}

/**
 * Disable `el` (a button, an input) when the caller lacks `p`: its title says
 * which permission it needs. Returns `el`.
 */
export function guard(el, p) {
  if (!el || can(p)) return el;
  el.disabled = true;
  el.title = reason(p);
  el.dataset.permission = p;
  el.classList.add('needs-permission');
  return el;
}
