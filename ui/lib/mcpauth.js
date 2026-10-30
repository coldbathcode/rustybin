// Authentication for the MCP inspector: the modes a real server asks for
// (none, bearer, API key in a header or the query, basic, custom headers,
// OAuth 2.1) as request headers, and the OAuth 2.1 walk as a timeline:
// the 401 challenge, the protected resource metadata (RFC 9728), the
// authorization server metadata (RFC 8414, else OpenID discovery), dynamic
// client registration (RFC 7591) when offered, then authorization code with
// PKCE in a popup (redirect to this console's /ui/callback) or client
// credentials, the token exchange and refresh. Documents are fetched from
// the browser, or through the instance (POST /_rustybin/mcp-relay/fetch) in
// relay mode or when the browser cannot reach them.
import { send, SERVER } from './http.js';
import { utf8, toBase64, randomString, pkceChallenge } from './crypto.js';
import { parseChallenge } from './mcpclient.js';

export const MODES = [
  ['none', 'No authentication'],
  ['bearer', 'Bearer token'],
  ['api_key', 'API key'],
  ['basic', 'Basic (user and password)'],
  ['headers', 'Custom headers'],
  ['oauth', 'OAuth 2.1 (discovery, PKCE)'],
];

/** Headers and query parameters of an auth state {kind, token, name, location, key, username, password, headers: [[k, v]]}. */
export function buildAuth(a) {
  const headers = [];
  const query = [];
  if (!a) return { headers, query };
  if ((a.kind === 'bearer' || a.kind === 'oauth') && a.token) headers.push(['Authorization', 'Bearer ' + a.token]);
  if (a.kind === 'api_key' && a.key) {
    const name = a.name || 'X-API-Key';
    if (a.location === 'query') query.push([name, a.key]); else headers.push([name, a.key]);
  }
  if (a.kind === 'basic') headers.push(['Authorization', 'Basic ' + toBase64(utf8((a.username || '') + ':' + (a.password || '')))]);
  if (a.kind === 'headers') for (const [k, v] of a.headers || []) if (k && v) headers.push([k, v]);
  return { headers, query };
}

/** A URL with query parameters appended. */
export function withQuery(url, query) {
  if (!query || !query.length) return url;
  const qs = query.map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&');
  return url + (url.includes('?') ? '&' : '?') + qs;
}

/**
 * Fetch one document: directly, or through the instance. Resolves
 * {status, json, text, headers: {name: value}, via}.
 */
export async function fetchDoc(url, opts = {}) {
  const abs = /^https?:\/\//i.test(url) ? url : SERVER + url;
  const sameOrigin = (() => { try { return new URL(abs).origin === location.origin; } catch { return false; } })();
  const tryDirect = opts.mode !== 'relay' || sameOrigin;
  if (tryDirect) {
    const r = await send({ method: opts.method || 'GET', url: abs, headers: opts.headers || [], body: opts.body, timeout: 15000, session: false });
    if (!r.error) {
      const hs = {};
      for (const [k, v] of r.headers) hs[k] = v;
      return { status: r.status, json: r.json, text: r.text, headers: hs, via: 'browser' };
    }
    if (opts.mode !== 'relay' && !opts.relayFallback) return { status: 0, json: null, text: '', headers: {}, via: 'browser', error: r.error };
  }
  const hmap = {};
  for (const [k, v] of opts.headers || []) hmap[k] = v;
  const target = sameOrigin ? new URL(abs).pathname + new URL(abs).search : abs;
  const r = await send({ method: 'POST', url: SERVER + '/_rustybin/mcp-relay/fetch', headers: [['Content-Type', 'application/json']], body: JSON.stringify({ url: target, method: opts.method || 'GET', headers: hmap, body: opts.body }), timeout: 30000, session: false });
  if (r.error || !r.ok) return { status: r.status, json: null, text: r.text, headers: {}, via: 'relay', error: r.error || (r.json && r.json.error) || ('HTTP ' + r.status) };
  return { status: r.json.status, json: r.json.json, text: r.json.body, headers: r.json.headers || {}, via: 'relay' };
}

/**
 * The OAuth 2.1 walk. `ctx`: {resource (the server URL), challenge (the
 * WWW-Authenticate value of the 401), mode, grant: 'authorization_code' |
 * 'client_credentials', clientId?, clientSecret?, scope?, step(n, title,
 * detail) -> {done(body), fail(msg)}}. Resolves {token, refresh, expiresIn,
 * tokenEndpoint, clientId, issuer, scope} or throws.
 */
export async function oauthWalk(ctx) {
  const step = ctx.step;
  const fetchOpts = { mode: ctx.mode, relayFallback: true };
  // 1. The challenge.
  const ch = parseChallenge(ctx.challenge || '');
  const s1 = step(1, 'The 401 challenge', 'WWW-Authenticate: ' + (ctx.challenge || '(missing: a gateway may not expose it through CORS)'));
  s1.done({ challenge: ch });
  // 2. Protected resource metadata (RFC 9728).
  const resUrl = new URL(/^https?:\/\//i.test(ctx.resource) ? ctx.resource : SERVER + ctx.resource);
  const prmUrl = ch.resource_metadata || (resUrl.origin + '/.well-known/oauth-protected-resource' + resUrl.pathname);
  const s2 = step(2, 'Protected resource metadata (RFC 9728)', 'GET ' + prmUrl);
  const prm = await fetchDoc(prmUrl, fetchOpts);
  if (!prm.json) { s2.fail(prm.error || ('answered ' + prm.status)); throw new Error('no protected resource metadata'); }
  s2.done(prm.json, prm.via);
  // 3. Authorization server metadata (RFC 8414, else OpenID discovery).
  const issuer = String((prm.json.authorization_servers || [resUrl.origin])[0]).replace(/\/$/, '');
  const iss = new URL(issuer);
  const candidates = [
    iss.origin + '/.well-known/oauth-authorization-server' + (iss.pathname === '/' ? '' : iss.pathname),
    iss.origin + '/.well-known/openid-configuration' + (iss.pathname === '/' ? '' : iss.pathname),
    issuer + '/.well-known/oauth-authorization-server',
    issuer + '/.well-known/openid-configuration',
  ];
  const s3 = step(3, 'Authorization server metadata (RFC 8414)', 'issuer ' + issuer);
  let asm = null;
  for (const u of [...new Set(candidates)]) {
    const r = await fetchDoc(u, fetchOpts);
    if (r.json && r.json.token_endpoint) { asm = r.json; s3.done(Object.assign({ fetched: u }, r.json), r.via); break; }
  }
  if (!asm) { s3.fail('no metadata at ' + candidates.join(', ')); throw new Error('no authorization server metadata'); }
  const scope = ctx.scope || (prm.json.scopes_supported || []).join(' ');
  const resource = prm.json.resource || resUrl.toString();
  const grant = ctx.grant || ((asm.grant_types_supported || []).includes('authorization_code') ? 'authorization_code' : 'client_credentials');
  // 4. The client: registered dynamically (RFC 7591) when offered, else given.
  let clientId = ctx.clientId || '';
  let clientSecret = ctx.clientSecret || '';
  const redirect = SERVER + '/ui/callback';
  if (!clientId && asm.registration_endpoint) {
    const s4 = step(4, 'Dynamic client registration (RFC 7591)', 'POST ' + asm.registration_endpoint);
    const meta = grant === 'client_credentials'
      ? { client_name: 'Rustybin MCP inspector', grant_types: ['client_credentials'], token_endpoint_auth_method: 'client_secret_post', scope }
      : { client_name: 'Rustybin MCP inspector', redirect_uris: [redirect], grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'], token_endpoint_auth_method: 'none', scope };
    const reg = await fetchDoc(asm.registration_endpoint, Object.assign({}, fetchOpts, { method: 'POST', headers: [['Content-Type', 'application/json']], body: JSON.stringify(meta) }));
    if (!reg.json || !reg.json.client_id) { s4.fail((reg.json && (reg.json.error_description || reg.json.error)) || reg.error || ('answered ' + reg.status)); throw new Error('registration failed'); }
    clientId = reg.json.client_id;
    clientSecret = reg.json.client_secret || clientSecret;
    s4.done(Object.assign({}, reg.json, reg.json.client_secret ? { client_secret: '(hidden)' } : {}), reg.via);
  } else {
    step(4, 'The client', clientId ? 'client_id ' + clientId : 'no registration endpoint: give a client id').done({ client_id: clientId || null });
    if (!clientId) throw new Error('the authorization server offers no registration: give a client id');
  }
  // 5. The grant.
  let tokenBody;
  if (grant === 'client_credentials') {
    const s5 = step(5, 'Client credentials grant', 'POST ' + asm.token_endpoint);
    tokenBody = new URLSearchParams({ grant_type: 'client_credentials', client_id: clientId, resource, scope });
    if (clientSecret) tokenBody.set('client_secret', clientSecret);
    s5.done({ grant_type: 'client_credentials', client_id: clientId, scope, resource });
  } else {
    const s5 = step(5, 'Authorization code with PKCE (popup)', 'GET ' + asm.authorization_endpoint);
    const verifier = randomString(32);
    const state = randomString(12);
    const q = new URLSearchParams({ response_type: 'code', client_id: clientId, redirect_uri: redirect, scope, state, resource, code_challenge: pkceChallenge(verifier), code_challenge_method: 'S256' });
    const url = asm.authorization_endpoint + (asm.authorization_endpoint.includes('?') ? '&' : '?') + q.toString();
    const code = await new Promise((resolve, reject) => {
      const popup = window.open(url, 'rustybin-mcp-oauth', 'width=520,height=700');
      if (!popup) { reject(new Error('the popup was blocked: allow popups for this console')); return; }
      const onMsg = (ev) => {
        if (ev.origin !== location.origin || !ev.data || ev.data.type !== 'rustybin-oauth-callback') return;
        window.removeEventListener('message', onMsg);
        clearInterval(timer);
        const p = new URLSearchParams(ev.data.search || '');
        if (p.get('state') !== state) { reject(new Error('the state does not match')); return; }
        if (p.get('error')) { reject(new Error(p.get('error') + (p.get('error_description') ? ': ' + p.get('error_description') : ''))); return; }
        resolve(p.get('code'));
      };
      window.addEventListener('message', onMsg);
      const timer = setInterval(() => { if (popup.closed) { clearInterval(timer); window.removeEventListener('message', onMsg); reject(new Error('the popup was closed before signing in')); } }, 500);
    }).catch((e) => { s5.fail(e.message); throw e; });
    s5.done({ code: code ? code.slice(0, 6) + '...' : null, state });
    tokenBody = new URLSearchParams({ grant_type: 'authorization_code', code, redirect_uri: redirect, client_id: clientId, code_verifier: verifier, resource });
  }
  // 6. The token.
  const s6 = step(6, 'Token request', 'POST ' + asm.token_endpoint);
  const tok = await fetchDoc(asm.token_endpoint, Object.assign({}, fetchOpts, { method: 'POST', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: tokenBody.toString() }));
  if (!tok.json || !tok.json.access_token) { s6.fail((tok.json && (tok.json.error_description || tok.json.error)) || tok.error || ('answered ' + tok.status)); throw new Error('no token'); }
  s6.done({ token_type: tok.json.token_type, expires_in: tok.json.expires_in, scope: tok.json.scope, refresh_token: tok.json.refresh_token ? '(kept)' : undefined }, tok.via);
  return { token: tok.json.access_token, refresh: tok.json.refresh_token || null, expiresIn: tok.json.expires_in, tokenEndpoint: asm.token_endpoint, clientId, clientSecret, issuer, scope, resource, mode: ctx.mode };
}

/** Refresh an OAuth token; resolves the new token response. */
export async function refreshToken(o) {
  const body = new URLSearchParams({ grant_type: 'refresh_token', refresh_token: o.refresh, client_id: o.clientId });
  if (o.clientSecret) body.set('client_secret', o.clientSecret);
  if (o.resource) body.set('resource', o.resource);
  const r = await fetchDoc(o.tokenEndpoint, { mode: o.mode, relayFallback: true, method: 'POST', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: body.toString() });
  if (!r.json || !r.json.access_token) throw new Error((r.json && (r.json.error_description || r.json.error)) || r.error || ('answered ' + r.status));
  return r.json;
}
