// OAuth popups without an opener. An authorization page (often a third
// party's, in the MCP inspector) must never reach the console's window
// through `window.opener` (reverse tabnabbing), so the popup is opened
// blank, its opener cut, then sent to the authorization page. The redirect
// lands on this console's /ui/callback, which has no opener to post to: it
// hands the query string back over a BroadcastChannel (same origin only),
// and the waiting flow takes it only when the `state` is its own, then
// acknowledges it. Without an acknowledgement (no flow waits: the
// same-window fallback of the token lab) the callback page keeps the query
// for the token lab instead.

export const CHANNEL = 'rustybin-oauth';
const CALLBACK = 'rustybin-oauth-callback';
const ACK = 'rustybin-oauth-ack';

/**
 * Open `url` in a new popup whose `window.opener` is null. Returns the
 * window (to see whether it was closed) or null when the browser blocked it.
 */
export function openPopup(url, features) {
  let w = null;
  try { w = window.open('', '_blank', features); } catch { w = null; }
  if (!w) return null;
  try {
    w.opener = null;
    w.location.replace(url);
  } catch {
    try { w.close(); } catch { /* ignore */ }
    return null;
  }
  return w;
}

/**
 * Wait for the callback of the flow with `state`. Resolves the callback's
 * URLSearchParams; rejects when the popup is closed first, on `signal`, or
 * after `timeoutMs` (10 minutes by default).
 */
export function waitForCallback(state, popup, opts = {}) {
  return new Promise((resolve, reject) => {
    let bc;
    try { bc = new BroadcastChannel(CHANNEL); } catch { reject(new Error('this browser cannot receive the sign-in (no BroadcastChannel)')); return; }
    let closedSince = 0;
    const finish = (fn, value) => {
      clearInterval(timer);
      clearTimeout(limit);
      if (opts.signal) opts.signal.removeEventListener('abort', onAbort);
      bc.close();
      fn(value);
    };
    bc.onmessage = (ev) => {
      const d = ev.data;
      if (!d || d.type !== CALLBACK || typeof d.search !== 'string') return;
      const p = new URLSearchParams(d.search);
      // Another flow's callback (another tab, an older popup): not ours.
      if (p.get('state') !== state) return;
      try { bc.postMessage({ type: ACK, id: d.id }); } catch { /* ignore */ }
      finish(resolve, p);
    };
    // A closed popup ends the wait (after a grace period: the callback may
    // still be on its way when the popup closes itself).
    const timer = setInterval(() => {
      if (!popup || !popup.closed) return;
      if (!closedSince) { closedSince = Date.now(); return; }
      if (Date.now() - closedSince > 1500) finish(reject, new Error('the popup was closed before signing in'));
    }, 500);
    const limit = setTimeout(() => finish(reject, new Error('no answer from the sign-in popup')), opts.timeoutMs || 600000);
    const onAbort = () => finish(reject, new Error('cancelled'));
    if (opts.signal) opts.signal.addEventListener('abort', onAbort);
  });
}

/**
 * The callback page's side: hand `search` to the waiting flow. Resolves
 * true when a flow acknowledged it, false after `waitMs` without one.
 */
export function deliverCallback(search, waitMs = 1500) {
  return new Promise((resolve) => {
    let bc;
    try { bc = new BroadcastChannel(CHANNEL); } catch { resolve(false); return; }
    const id = Math.random().toString(36).slice(2) + Date.now().toString(36);
    const timer = setTimeout(() => { bc.close(); resolve(false); }, waitMs);
    bc.onmessage = (ev) => {
      if (!ev.data || ev.data.type !== ACK || ev.data.id !== id) return;
      clearTimeout(timer);
      bc.close();
      resolve(true);
    };
    bc.postMessage({ type: CALLBACK, search, id });
  });
}
