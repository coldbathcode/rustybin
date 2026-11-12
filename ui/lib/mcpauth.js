// Authentication for the MCP inspector: the modes a real server asks for
// (none, bearer, API key in a header or the query, basic, custom headers,
// OAuth 2.1) as request headers, and the OAuth 2.1 walk as a timeline:
// the 401 challenge, the protected resource metadata (RFC 9728), the
// authorization server metadata (RFC 8414, else OpenID discovery), the
// client (a given id, this instance's client ID metadata document where the
// server supports one, else dynamic registration, RFC 7591), then
// authorization code with PKCE in a popup (redirect to this console's /ui/callback) or client
// credentials, the token exchange and refresh. Documents are fetched from
// the browser, or through the instance (POST /_rustybin/mcp-relay/fetch) in
// relay mode or when the browser cannot reach them.
import { send, SERVER, isControlUrl } from './http.js';
import { webUrl } from './dom.js';
import { utf8, toBase64, randomString, pkceChallenge } from './crypto.js';
import { parseChallenge } from './mcpclient.js';
import { openPopup, waitForCallback } from './oauthpopup.js';

export const MODES = [
  ['none', 'No authentication'],
  ['bearer', 'Bearer token'],
  ['api_key', 'API key'],
  ['basic', 'Basic (user and password)'],
  ['headers', 'Custom headers'],
  ['oauth', 'OAuth 2.1 (discovery, PKCE)'],
];

/** How the walk names its client (the auth form's choice; `auth.oauth.client` of a saved connection). */
export const CLIENT_WAYS = [
  ['auto', 'Automatic: a given id, a document, registration'],
  ['document', 'Client ID metadata document'],
  ['registration', 'Dynamic registration (RFC 7591)'],
  ['given', 'The client id below'],
];

/**
 * Which client the walk uses, and why not the others. Pure: `way` (one of
 * CLIENT_WAYS, default auto), `clientId` (what the user gave), `grant`,
 * `asm` (the authorization server's metadata), `doc` (this instance's
 * client document as GET /_rustybin/mcp-servers answers it: {url, usable,
 * reason, document}), `redirect` (the callback this console listens on).
 * Returns {use: 'given' | 'document' | 'registration' | 'stop', why: [..],
 * error?}. Automatic order: a given id, a document when the server supports
 * one and this instance can offer its own, registration, then asking. A
 * forced way is tried even where the server does not advertise it, to test
 * the server, and stops only where it cannot be tried at all.
 */
export function chooseClient({ way, clientId, grant, asm, doc, redirect }) {
  const why = [];
  const uris = doc && doc.document && Array.isArray(doc.document.redirect_uris) ? doc.document.redirect_uris : [];
  // What makes a document impossible here, whatever the server says.
  const cannot = grant === 'client_credentials' ? 'a document describes a public client, which has no client credentials'
    : !doc || !doc.url ? 'this instance did not say where its client document is'
      : !uris.includes(redirect) ? 'the document\'s callback (' + (uris[0] || 'none') + ') is not this console\'s (' + redirect + '): open the console at the instance\'s own address'
        : null;
  // What makes it unlikely to work: tried only when forced.
  const unlikely = !(asm && asm.client_id_metadata_document_supported === true) ? 'the authorization server does not say client_id_metadata_document_supported'
    : doc && !doc.usable ? String(doc.reason || 'this instance\'s document cannot be fetched by the server')
      : null;
  const registers = !!(asm && asm.registration_endpoint);
  const stop = (error) => ({ use: 'stop', why, error });
  if (way === 'given') return clientId ? { use: 'given', why: ['forced'] } : stop('give a client id (the client was set to the given id)');
  if (way === 'document') {
    if (cannot) return stop('a client ID metadata document cannot be used: ' + cannot);
    if (unlikely) why.push('forced, although ' + unlikely);
    else why.push('forced');
    return { use: 'document', why };
  }
  if (way === 'registration') {
    if (!registers) return stop('the authorization server names no registration_endpoint');
    return { use: 'registration', why: ['forced'] };
  }
  if (clientId) return { use: 'given', why: ['a client id was given'] };
  const noDocument = cannot || unlikely;
  if (!noDocument) return { use: 'document', why: ['the server supports client ID metadata documents: no registration call'] };
  why.push('no document: ' + noDocument);
  if (registers) return { use: 'registration', why };
  why.push('no registration: the authorization server names no registration_endpoint');
  return stop('give a client id');
}

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
/** True for a host name of this computer (`localhost`, `127.x.y.z`, `[::1]`, `*.localhost`). */
function isLoopbackHost(host) {
  const h = String(host || '').toLowerCase();
  return h === 'localhost' || h.endsWith('.localhost') || h === '[::1]' || /^127\.\d+\.\d+\.\d+$/.test(h);
}

/** This instance's OAuth sign-in endpoints: the only control paths a discovery document may name. */
const OWN_OAUTH = ['/_rustybin/oauth/register', '/_rustybin/oauth/authorize', '/_rustybin/oauth/token'];

/**
 * Why a URL from a discovery document may not be fetched, or null. These
 * URLs come from the server being inspected (`resource`, the URL the user
 * chose): only absolute `https:` (or `http:` on a loopback host or this
 * instance's own origin); a loopback host only when the inspected server is
 * itself on loopback (a remote server's documents may not point the browser
 * at this computer); and never this instance's control plane, its OAuth
 * sign-in endpoints aside (exact paths).
 */
export function untrustedUrlProblem(url, resource) {
  let u;
  try { u = new URL(String(url)); } catch { return 'not an absolute URL: ' + String(url).slice(0, 80); }
  const base = new URL(SERVER + '/');
  const ours = u.origin === base.origin;
  const loopback = isLoopbackHost(u.hostname);
  if (u.protocol !== 'https:' && !(u.protocol === 'http:' && (loopback || ours))) return 'only https URLs are followed (' + u.protocol + ' refused)';
  if (loopback && !ours) {
    let local = false;
    try { local = isLoopbackHost(new URL(String(resource || '')).hostname); } catch { local = false; }
    if (!local) return 'a remote server\'s document points at this computer (' + u.host + '): refused';
  }
  if (isControlUrl(u.toString())) {
    const root = base.pathname.replace(/\/$/, '');
    if (!ours || !OWN_OAUTH.some((p) => u.pathname === root + p)) return 'the document points at this instance\'s control plane: refused';
  }
  return null;
}

export async function fetchDoc(url, opts = {}) {
  const problem = untrustedUrlProblem(url, opts.resource);
  if (problem) return { status: 0, json: null, text: '', headers: {}, via: 'browser', error: problem };
  const abs = url;
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
  const r = await send({ control: true, method: 'POST', url: SERVER + '/_rustybin/mcp-relay/fetch', headers: [['Content-Type', 'application/json']], body: JSON.stringify({ url: target, method: opts.method || 'GET', headers: hmap, body: opts.body }), timeout: 30000, session: false });
  if (r.error || !r.ok) return { status: r.status, json: null, text: r.text, headers: {}, via: 'relay', error: r.error || (r.json && r.json.error) || ('HTTP ' + r.status) };
  return { status: r.json.status, json: r.json.json, text: r.json.body, headers: r.json.headers || {}, via: 'relay' };
}

/**
 * The OAuth 2.1 walk. `ctx`: {resource (the server URL), challenge (the
 * WWW-Authenticate value of the 401), mode, grant: 'authorization_code' |
 * 'client_credentials', clientId?, clientSecret?, scope?, clientWay? (one of
 * CLIENT_WAYS), clientDocument? (this instance's, for chooseClient),
 * step(n, title, detail) -> {done(body), fail(msg)}}. Resolves {token,
 * refresh, expiresIn, tokenEndpoint, clientId, clientWay (the way used),
 * issuer, scope} or throws.
 */
export async function oauthWalk(ctx) {
  const step = ctx.step;
  // 1. The challenge.
  const ch = parseChallenge(ctx.challenge || '');
  const s1 = step(1, 'The 401 challenge', 'WWW-Authenticate: ' + (ctx.challenge || '(missing: a gateway may not expose it through CORS)'));
  s1.done({ challenge: ch });
  // 2. Protected resource metadata (RFC 9728).
  const resUrl = new URL(/^https?:\/\//i.test(ctx.resource) ? ctx.resource : SERVER + ctx.resource);
  // The server the user chose decides whether loopback documents are followed
  // (the documents' own `resource` is the server's to say).
  const fetchOpts = { mode: ctx.mode, relayFallback: true, resource: resUrl.toString() };
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
  // 4. The client: a given id, this instance's metadata document where the
  // server supports one, dynamic registration (RFC 7591), else asking. The
  // step says which was used and why the others were not.
  let clientId = ctx.clientId || '';
  let clientSecret = ctx.clientSecret || '';
  const redirect = SERVER + '/ui/callback';
  const choice = chooseClient({ way: ctx.clientWay || 'auto', clientId, grant, asm, doc: ctx.clientDocument, redirect });
  const reasons = choice.why.length ? ' (' + choice.why.join('; ') + ')' : '';
  if (choice.use === 'stop') {
    step(4, 'The client', 'no client' + reasons).fail(choice.error);
    throw new Error(choice.error);
  }
  if (choice.use === 'document') {
    // The document's URL is the client id: nothing is registered, the
    // authorization server fetches the document itself.
    clientId = ctx.clientDocument.url;
    clientSecret = '';
    step(4, 'Client ID metadata document', 'client_id ' + clientId + reasons).done(ctx.clientDocument.document);
  } else if (choice.use === 'registration') {
    if (!asm.registration_endpoint) throw new Error('no registration endpoint');
    const s4 = step(4, 'Dynamic client registration (RFC 7591)', 'POST ' + asm.registration_endpoint + reasons);
    const meta = grant === 'client_credentials'
      ? { client_name: 'Rustybin MCP inspector', grant_types: ['client_credentials'], token_endpoint_auth_method: 'client_secret_post', scope }
      : { client_name: 'Rustybin MCP inspector', redirect_uris: [redirect], grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'], token_endpoint_auth_method: 'none', scope };
    const reg = await fetchDoc(asm.registration_endpoint, Object.assign({}, fetchOpts, { method: 'POST', headers: [['Content-Type', 'application/json']], body: JSON.stringify(meta) }));
    if (!reg.json || !reg.json.client_id) { s4.fail((reg.json && (reg.json.error_description || reg.json.error)) || reg.error || ('answered ' + reg.status)); throw new Error('registration failed'); }
    clientId = reg.json.client_id;
    clientSecret = reg.json.client_secret || clientSecret;
    s4.done(Object.assign({}, reg.json, reg.json.client_secret ? { client_secret: '(hidden)' } : {}), reg.via);
  } else {
    step(4, 'The client', 'client_id ' + clientId + reasons).done({ client_id: clientId });
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
    const authProblem = untrustedUrlProblem(asm.authorization_endpoint, resUrl.toString());
    if (authProblem) { s5.fail(authProblem); throw new Error(authProblem); }
    // The authorization page is a third party's: the popup has no opener
    // (no reverse tabnabbing), and the code comes back from /ui/callback
    // over a BroadcastChannel, taken only with this flow's state.
    const code = await (async () => {
      const popup = webUrl(url) ? openPopup(url, 'width=520,height=700') : null;
      if (!popup) throw new Error('the popup was blocked: allow popups for this console');
      const p = await waitForCallback(state, popup);
      if (p.get('error')) throw new Error(p.get('error') + (p.get('error_description') ? ': ' + p.get('error_description') : ''));
      if (!p.get('code')) throw new Error('the callback carried no code');
      return p.get('code');
    })().catch((e) => { s5.fail(e.message); throw e; });
    s5.done({ code: code ? code.slice(0, 6) + '...' : null, state });
    tokenBody = new URLSearchParams({ grant_type: 'authorization_code', code, redirect_uri: redirect, client_id: clientId, code_verifier: verifier, resource });
  }
  // 6. The token.
  const s6 = step(6, 'Token request', 'POST ' + asm.token_endpoint);
  const tok = await fetchDoc(asm.token_endpoint, Object.assign({}, fetchOpts, { method: 'POST', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: tokenBody.toString() }));
  if (!tok.json || !tok.json.access_token) { s6.fail((tok.json && (tok.json.error_description || tok.json.error)) || tok.error || ('answered ' + tok.status)); throw new Error('no token'); }
  s6.done({ token_type: tok.json.token_type, expires_in: tok.json.expires_in, scope: tok.json.scope, refresh_token: tok.json.refresh_token ? '(kept)' : undefined }, tok.via);
  return { token: tok.json.access_token, refresh: tok.json.refresh_token || null, expiresIn: tok.json.expires_in, tokenEndpoint: asm.token_endpoint, clientId, clientWay: choice.use, clientSecret, issuer, scope, resource, target: resUrl.toString(), mode: ctx.mode };
}

/** Refresh an OAuth token; resolves the new token response. */
export async function refreshToken(o) {
  const body = new URLSearchParams({ grant_type: 'refresh_token', refresh_token: o.refresh, client_id: o.clientId });
  if (o.clientSecret) body.set('client_secret', o.clientSecret);
  if (o.resource) body.set('resource', o.resource);
  const r = await fetchDoc(o.tokenEndpoint, { mode: o.mode, relayFallback: true, resource: o.target, method: 'POST', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: body.toString() });
  if (!r.json || !r.json.access_token) throw new Error((r.json && (r.json.error_description || r.json.error)) || r.error || ('answered ' + r.status));
  return r.json;
}
