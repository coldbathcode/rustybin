// Self-service visits: the manage tokens of the visitor's mocks.
//
// A visitor without the admin token owns the mocks they create through a
// manage token (rbm_...). The console keeps the tokens in localStorage (so a
// closed tab keeps the visit), sends them as X-Rustybin-Mock-Token on calls to
// /_rustybin/mocks* (see http.js) and accepts manage links
// (#/mocks&mock_token=...; fragments never reach the server), stripping the
// token from the address bar at once.

const KEY = 'rustybin.console.mockTokens';
const MAX = 20;
export const HEADER = 'X-Rustybin-Mock-Token';

function load() {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || '[]');
    return Array.isArray(v) ? v.filter((t) => typeof t === 'string' && /^rbm_[a-z2-7]{32}$/.test(t)).slice(0, MAX) : [];
  } catch {
    return [];
  }
}

let list = load();

function save() {
  try { localStorage.setItem(KEY, JSON.stringify(list)); } catch { /* storage unavailable: memory only */ }
}

/** The manage tokens held by this browser, newest first. */
export function tokens() {
  return list.slice();
}

/** Keep a token (newest first). */
export function add(token) {
  const t = String(token || '').trim();
  if (!/^rbm_[a-z2-7]{32}$/.test(t)) return false;
  list = [t, ...list.filter((x) => x !== t)].slice(0, MAX);
  save();
  return true;
}

/** Replace a token (rotated). */
export function replaceToken(oldToken, newToken) {
  list = list.filter((x) => x !== oldToken);
  add(newToken);
}

/** Forget a token on this browser (the visit keeps running). */
export function remove(token) {
  list = list.filter((x) => x !== token);
  save();
}

/** Forget every token. */
export function clearAll() {
  list = [];
  save();
}

/** A manage link for `token` (opens the mocks list and keeps the token). */
export function manageLink(token) {
  return location.origin + location.pathname + '#/mocks&mock_token=' + encodeURIComponent(token);
}

/** Move `&mock_token=...` from the address bar into storage. */
export function consumeFragment() {
  const raw = (location.hash || '').replace(/^#/, '');
  if (!/(^|&)mock_token=/.test(raw)) return false;
  const rest = [];
  for (const part of raw.split('&')) {
    const m = /^mock_token=(.*)$/.exec(part);
    if (m) {
      let t = m[1];
      try { t = decodeURIComponent(t); } catch { /* keep raw */ }
      add(t);
    } else if (part) {
      rest.push(part);
    }
  }
  const hash = rest.length ? '#' + rest.join('&') : '#/mocks';
  try { history.replaceState(history.state, '', location.pathname + location.search + hash); } catch { location.hash = hash; }
  return true;
}

/** "in 42 min" / "in 1 h 5 min" for a number of seconds. */
export function remaining(secs) {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return s + ' s';
  const m = Math.floor(s / 60);
  if (m < 60) return m + ' min';
  return Math.floor(m / 60) + ' h ' + (m % 60) + ' min';
}

/** "4 of 6 hours" for minutes. */
export function hours(used, allowed) {
  const f = (m) => (m % 60 === 0 ? String(m / 60) : (m / 60).toFixed(1));
  return `${f(used)} of ${f(allowed)} hours`;
}
