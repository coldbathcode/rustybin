// Projects (access control step 11): the projects the caller may see
// (GET /_rustybin/projects) and the one the top bar's picker chose. The
// views that list project resources (mocks, saved tests, checkpoints) pass
// it as ?project=, and new mocks, tests and checkpoints go to it. The choice
// lives in the tab (sessionStorage).

import { send, SERVER } from './http.js';
import * as auth from './auth.js';

const KEY = 'rustybin.project';
/** The picker's value for the instance's resources (never a project's name). */
export const INSTANCE = 'instance';

let list = [];

/** Load the projects; resolves true when the list changed. */
export async function load() {
  const before = JSON.stringify(list);
  if (auth.required() && auth.needsSignIn()) {
    list = [];
  } else {
    const r = await send({ control: true, url: SERVER + '/_rustybin/projects', session: false, timeout: 15000 });
    list = r.ok && r.json && Array.isArray(r.json.projects) ? r.json.projects : [];
  }
  const cur = current();
  if (cur && cur !== INSTANCE && !list.some((p) => p.name === cur)) setCurrent('');
  return JSON.stringify(list) !== before;
}

/** The projects the caller may see. */
export function all() {
  return list.slice();
}

/** The chosen project's name, INSTANCE, or '' (everything). */
export function current() {
  try { return sessionStorage.getItem(KEY) || ''; } catch { return ''; }
}

export function setCurrent(name) {
  try {
    if (name) sessionStorage.setItem(KEY, name);
    else sessionStorage.removeItem(KEY);
  } catch { /* private mode: the choice lasts this page */ }
}

/** `?project=<chosen>` (or `&...` with `sep`), '' when everything is shown. */
export function query(sep = '?') {
  const c = current();
  return c ? sep + 'project=' + encodeURIComponent(c) : '';
}

/** The project a new resource goes to: the chosen one, else none (the server picks). */
export function forNew() {
  const c = current();
  return c || undefined;
}
