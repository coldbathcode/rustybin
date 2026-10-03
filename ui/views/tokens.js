// Token lab: tokens from the built-in IdP (client_credentials, password,
// authorization code + PKCE), a JWT decoder with expiry countdown,
// introspection, userinfo and JWKS, plus an HMAC request signer for
// /auth/hmac and a Standard Webhooks signer / verifier.
import { h, replace, field, input, textarea, select, notice, icon, badge, segmented, spinner, toast, kvgrid, tabs, checkbox, copyButton } from '../lib/dom.js';
import { jsonView, codeBlock, copyableCode, headersTable } from '../lib/format.js';
import { SERVER, send, toCurl } from '../lib/http.js';
import { responseView } from '../lib/widgets.js';
import { decodeJwt, randomString, pkceChallenge, hmacSha256, sha256, utf8, toBase64 } from '../lib/crypto.js';
import * as settings from '../lib/settings.js';

const USERS = [['demo', 'demo / demo'], ['alice', 'alice / alice (admins)'], ['bob', 'bob / bob']];
const PENDING_KEY = 'rustybin.oauth.pending';

export default {
  id: 'tokens',
  mount(root) {
    let alive = true;
    let countdown = null;
    let oauthOut = null;
    const st = settings.recall('tokens.state', { grant: 'client_credentials', clientId: 'rustybin', clientSecret: 'secret', scope: 'openid profile email', resource: '', user: 'alice', tokenResponse: null, jwt: '' });
    settings.remember('tokens.state', st);

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Token lab'),
        h('p', null, 'Get tokens from the built-in OAuth 2.0 / OIDC provider, inspect them, and sign requests: everything a presenter needs for JWT, OIDC, HMAC and webhook signature plugins.'))));

    const t = tabs([
      { id: 'oauth', label: 'OAuth tokens', render: oauthPane },
      { id: 'jwt', label: 'JWT decoder', render: jwtPane },
      { id: 'hmac', label: 'HMAC signer', render: hmacPane },
      { id: 'webhooks', label: 'Webhook signatures', render: webhookPane },
    ], settings.recall('tokens.tab', 'oauth'), (id) => settings.remember('tokens.tab', id));
    root.appendChild(h('div', { class: 'card' }, t.el));

    // ── OAuth ──
    function oauthPane() {
      const out = h('div');
      oauthOut = out;
      const grantSeg = segmented([['client_credentials', 'Client credentials'], ['password', 'Password'], ['code', 'Authorization code + PKCE']], st.grant, (v) => { st.grant = v; renderFields(); }, 'Grant type');
      const fields = h('div');
      const goBtn = h('button', { class: 'btn primary', type: 'button' }, icon('token'), 'Get token');
      const clientId = input({ value: st.clientId, class: 'input mono' });
      const clientSecret = input({ value: st.clientSecret, class: 'input mono' });
      const scope = input({ value: st.scope, class: 'input mono' });
      const resource = input({ value: st.resource, class: 'input mono', placeholder: 'optional, e.g. https://api.example.com' });
      const user = select(USERS, st.user);
      const sync = () => { st.clientId = clientId.value.trim(); st.clientSecret = clientSecret.value; st.scope = scope.value.trim(); st.resource = resource.value.trim(); st.user = user.value; };
      for (const el of [clientId, clientSecret, scope, resource]) el.addEventListener('input', sync);
      user.addEventListener('change', sync);

      function renderFields() {
        if (st.grant === 'code') {
          replace(fields, h('div', { class: 'stack-sm' },
            notice('', 'Opens the IdP login page in a popup (public client ', h('code', null, 'rustybin-public'), ', PKCE S256). Sign in with a demo user; the code comes back to ', h('code', null, SERVER + '/ui/callback'), ' and is exchanged here.'),
            h('div', { class: 'grid grid-2' }, field('Scope', scope), field('Resource (audience)', resource))));
          goBtn.lastChild.textContent = 'Sign in';
        } else {
          replace(fields, h('div', { class: 'grid grid-2' },
            field('Client id', clientId), field('Client secret', clientSecret),
            st.grant === 'password' ? field('Demo user', user) : null,
            field('Scope', scope), field('Resource (audience)', resource, 'RFC 8707: sets the aud claim (default rustybin)')));
          goBtn.lastChild.textContent = 'Get token';
        }
      }
      renderFields();

      goBtn.addEventListener('click', async () => {
        sync();
        if (st.grant === 'code') { startCodeFlow(out); return; }
        const p = new URLSearchParams({ grant_type: st.grant, client_id: st.clientId, client_secret: st.clientSecret });
        if (st.scope) p.set('scope', st.scope);
        if (st.resource) p.set('resource', st.resource);
        if (st.grant === 'password') { p.set('username', st.user); p.set('password', st.user); }
        await tokenRequest(p, out);
      });

      if (st.tokenResponse) renderTokenResponse(out, st.tokenResponse);
      return h('div', { class: 'card-body stack' }, h('div', { class: 'row' }, grantSeg.el), fields, h('div', { class: 'row' }, goBtn), out);
    }

    async function tokenRequest(params, out) {
      replace(out, spinner('POST /oauth/token'));
      const req = { method: 'POST', url: SERVER + '/oauth/token', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: params.toString(), timeout: 15000 };
      const r = await send(req);
      if (!alive) return;
      if (r.json && r.json.access_token) {
        st.tokenResponse = { json: r.json, curl: toCurl(req), at: Date.now() };
        st.jwt = r.json.access_token;
        renderTokenResponse(out, st.tokenResponse);
        toast('Token issued');
      } else {
        replace(out, responseView(r));
      }
    }

    function renderTokenResponse(out, tr) {
      const j = tr.json;
      const claims = decodeJwt(j.access_token);
      replace(out, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, badge('token_type ' + j.token_type, 'ok'), j.expires_in ? badge('expires_in ' + j.expires_in + ' s', 'outline') : null, j.scope ? badge('scope: ' + j.scope, 'outline') : null),
        claims ? kvgrid([['sub', claims.payload.sub], ['aud', JSON.stringify(claims.payload.aud)], ['iss', claims.payload.iss], ['kid', claims.header.kid]]) : null,
        h('div', { class: 'row' },
          h('button', { class: 'btn sm primary', type: 'button', onclick: () => { st.jwt = j.access_token; settings.remember('tokens.tab', 'jwt'); t.select('jwt'); } }, 'Decode access token'),
          j.id_token ? h('button', { class: 'btn sm', type: 'button', onclick: () => { st.jwt = j.id_token; t.select('jwt'); } }, 'Decode ID token') : null,
          copyButton(j.access_token, { label: 'Copy access token' }),
          copyButton('Authorization: Bearer ' + j.access_token, { label: 'Copy header' })),
        h('details', { class: 'disclosure' }, h('summary', null, 'Token response'), jsonView(j, 'short')),
        h('details', { class: 'disclosure' }, h('summary', null, 'curl'), copyableCode(tr.curl))));
    }

    async function startCodeFlow(out) {
      const verifier = randomString(48);
      const state = randomString(12);
      const nonce = randomString(12);
      const redirect = SERVER + '/ui/callback';
      const p = new URLSearchParams({ response_type: 'code', client_id: 'rustybin-public', redirect_uri: redirect, scope: st.scope || 'openid', state, nonce, code_challenge: pkceChallenge(verifier), code_challenge_method: 'S256' });
      if (st.resource) p.set('resource', st.resource);
      const pending = { verifier, state, redirect };
      try { sessionStorage.setItem(PENDING_KEY, JSON.stringify(pending)); } catch { /* ignore */ }
      const url = SERVER + '/oauth/authorize?' + p.toString();
      replace(out, h('div', { class: 'stack-sm' }, spinner('Waiting for the login popup'), h('code', { class: 'small break' }, url)));
      const popup = window.open(url, 'rustybin-login', 'width=520,height=680');
      if (!popup) {
        replace(out, notice('warn', 'The popup was blocked. ', h('a', { href: url }, 'Continue in this window'), ' (you come back here after signing in).'));
        return;
      }
      const onMsg = (ev) => {
        if (ev.origin !== location.origin || !ev.data || ev.data.type !== 'rustybin-oauth-callback') return;
        window.removeEventListener('message', onMsg);
        finishCodeFlow(ev.data.search, out);
      };
      window.addEventListener('message', onMsg);
      const watch = setInterval(() => {
        if (!alive) { clearInterval(watch); window.removeEventListener('message', onMsg); return; }
        if (popup.closed) {
          clearInterval(watch);
          setTimeout(() => { window.removeEventListener('message', onMsg); }, 1000);
          if (out.querySelector('.spinner')) replace(out, notice('warn', 'The login window was closed before the sign in finished.'));
        }
      }, 700);
    }

    async function finishCodeFlow(search, out) {
      const q = new URLSearchParams(search || '');
      let pending = null;
      try { pending = JSON.parse(sessionStorage.getItem(PENDING_KEY) || 'null'); sessionStorage.removeItem(PENDING_KEY); } catch { pending = null; }
      if (q.get('error')) { replace(out, notice('err', `${q.get('error')}: ${q.get('error_description') || ''}`)); return; }
      if (!pending || !q.get('code')) { replace(out, notice('err', 'No pending authorization request in this browser tab.')); return; }
      if (q.get('state') !== pending.state) { replace(out, notice('err', 'The state parameter does not match (possible CSRF): the code was not exchanged.')); return; }
      const p = new URLSearchParams({ grant_type: 'authorization_code', code: q.get('code'), redirect_uri: pending.redirect, client_id: 'rustybin-public', code_verifier: pending.verifier });
      await tokenRequest(p, out);
    }

    // Same-window fallback: app.js stored the callback query string.
    let stored = null;
    try { stored = sessionStorage.getItem('rustybin.oauth.callback'); sessionStorage.removeItem('rustybin.oauth.callback'); } catch { stored = null; }
    if (stored) {
      st.grant = 'code';
      t.select('oauth');
      if (oauthOut) finishCodeFlow(stored, oauthOut);
    }

    // ── JWT decoder ──
    function jwtPane() {
      const tokenIn = textarea({ rows: 4, placeholder: 'Paste a JWT (header.payload.signature)', value: st.jwt });
      const out = h('div');
      tokenIn.addEventListener('input', () => { st.jwt = tokenIn.value.trim(); render(); });
      const extra = h('div');
      function render() {
        clearInterval(countdown);
        const d = decodeJwt(st.jwt);
        if (!st.jwt) { replace(out, h('p', { class: 'small muted' }, 'Get a token in the OAuth tab, or paste one.')); return; }
        if (!d) { replace(out, notice('err', 'Not a decodable JWT (three base64url parts with JSON header and payload).')); return; }
        const colored = h('div', { class: 'token-box' }, h('span', { class: 't-h' }, d.parts[0]), '.', h('span', { class: 't-p' }, d.parts[1]), '.', h('span', { class: 't-s' }, d.parts[2]));
        const cd = h('span', { class: 'countdown' });
        const exp = d.payload.exp;
        const tick = () => {
          if (!exp) { cd.textContent = 'no exp claim'; return; }
          const left = Math.round(exp - Date.now() / 1000);
          cd.textContent = left > 0 ? `${Math.floor(left / 60)}m ${String(left % 60).padStart(2, '0')}s left` : `expired ${-left} s ago`;
          cd.style.color = left > 60 ? 'var(--ok)' : left > 0 ? 'var(--warn)' : 'var(--err)';
        };
        tick();
        countdown = setInterval(tick, 1000);
        const time = (v) => (v ? `${v} (${new Date(v * 1000).toLocaleString()})` : undefined);
        replace(out, h('div', { class: 'stack-sm' }, colored,
          h('div', { class: 'row' }, cd, d.header.alg ? badge('alg ' + d.header.alg, 'outline') : null, d.header.kid ? badge('kid ' + d.header.kid, 'outline') : null),
          kvgrid([['iss', d.payload.iss], ['sub', d.payload.sub], ['aud', d.payload.aud !== undefined ? JSON.stringify(d.payload.aud) : undefined], ['scope', d.payload.scope], ['iat', time(d.payload.iat)], ['exp', time(d.payload.exp)]]),
          h('div', { class: 'grid grid-2' }, h('div', null, h('span', { class: 'label' }, 'Header'), jsonView(d.header, 'short')), h('div', null, h('span', { class: 'label' }, 'Claims'), jsonView(d.payload, 'short')))));
      }
      render();
      const action = (label, fn) => h('button', { class: 'btn sm', type: 'button', onclick: fn }, label);
      return h('div', { class: 'card-body stack' }, tokenIn, out,
        h('div', { class: 'row' },
          action('Introspect', async () => { replace(extra, spinner('POST /oauth/introspect')); const r = await send({ method: 'POST', url: SERVER + '/oauth/introspect', headers: [['Content-Type', 'application/x-www-form-urlencoded'], ['Authorization', 'Basic ' + btoa('rustybin:secret')]], body: new URLSearchParams({ token: st.jwt }).toString() }); if (alive) replace(extra, responseView(r)); }),
          action('Userinfo', async () => { replace(extra, spinner('GET /oauth/userinfo')); const r = await send({ url: SERVER + '/oauth/userinfo', headers: [['Authorization', 'Bearer ' + st.jwt]] }); if (alive) replace(extra, responseView(r)); }),
          action('Validate at /auth/jwt', async () => { replace(extra, spinner('GET /auth/jwt')); const r = await send({ url: SERVER + '/auth/jwt', headers: [['Authorization', 'Bearer ' + st.jwt]] }); if (alive) replace(extra, responseView(r)); }),
          action('Show JWKS', async () => { replace(extra, spinner('GET /oauth/jwks')); const r = await send({ url: SERVER + '/oauth/jwks' }); if (alive) replace(extra, responseView(r)); })),
        extra);
    }

    // ── HMAC signer ──
    function hmacPane() {
      const user = input({ value: 'alice', class: 'input mono' });
      const secret = input({ value: 'secret', class: 'input mono' });
      const method = select(['GET', 'POST', 'PUT', 'DELETE'], 'POST', { style: { width: '110px' } });
      const path = input({ value: '/auth/hmac?demo=1', class: 'input mono' });
      const body = textarea({ rows: 3 });
      body.value = '{"order": 42}';
      const signTarget = segmented([['(request-target)', '(request-target)'], ['request-line', 'request-line']], '(request-target)', () => compute(), 'Pseudo header');
      const digestCb = checkbox('Sign a Digest of the body', true, () => compute());
      const preview = h('div');
      const out = h('div');
      let current = null;
      function compute() {
        const m = method.value;
        const p = path.value.trim() || '/auth/hmac';
        const xdate = new Date().toUTCString();
        const lines = [`x-date: ${xdate}`];
        const signed = ['x-date'];
        if (signTarget.get() === 'request-line') { lines.push(`${m} ${p} HTTP/1.1`); signed.push('request-line'); }
        else { lines.push(`(request-target): ${m.toLowerCase()} ${p}`); signed.push('(request-target)'); }
        const headers = [['X-Date', xdate]];
        const hasBody = m !== 'GET' && body.value;
        if (digestCb.input.checked && hasBody) {
          const digest = 'SHA-256=' + toBase64(sha256(utf8(body.value)));
          lines.push(`digest: ${digest}`);
          signed.push('digest');
          headers.push(['Digest', digest]);
        }
        const signingString = lines.join('\n');
        const signature = toBase64(hmacSha256(secret.value, signingString));
        const auth = `hmac username="${user.value}", algorithm="hmac-sha256", headers="${signed.join(' ')}", signature="${signature}"`;
        headers.push(['Authorization', auth]);
        if (hasBody) headers.push(['Content-Type', 'application/json']);
        current = { method: m, url: SERVER + p, headers, body: hasBody ? body.value : undefined };
        replace(preview, h('div', { class: 'stack-sm' },
          h('span', { class: 'label' }, 'Signing string'), codeBlock(signingString, null, 'short'),
          h('span', { class: 'label' }, 'Request headers'), headersTable(headers, { noHighlight: true }),
          copyableCode(toCurl(current))));
      }
      for (const el of [user, secret, path, body]) el.addEventListener('input', compute);
      method.addEventListener('change', compute);
      compute();
      const sendBtn = h('button', { class: 'btn primary', type: 'button', onclick: async () => { compute(); replace(out, spinner('Sending')); const r = await send(current); if (alive) replace(out, responseView(r)); } }, icon('send'), 'Sign and send');
      return h('div', { class: 'card-body stack' },
        notice('', 'Signs like a gateway hmac-auth plugin (draft-cavage style). Browsers cannot set the Date header, so X-Date is signed instead. Default credentials: alice / secret. The secret never leaves the browser; the signature is computed here.'),
        h('div', { class: 'grid grid-4' }, field('Username', user), field('Secret', secret), h('div', { class: 'field' }, h('span', null, 'Pseudo header'), signTarget.el), h('div', { class: 'field' }, h('span', null, 'Body'), digestCb.el)),
        h('div', { class: 'row nowrap' }, method, h('div', { class: 'grow' }, path), sendBtn),
        field('Body', body), preview, out);
    }

    // ── Webhooks ──
    function webhookPane() {
      const scheme = select([['standard', 'Standard Webhooks'], ['github', 'GitHub (X-Hub-Signature-256)'], ['stripe', 'Stripe (Stripe-Signature)']], 'standard');
      const secret = input({ value: 'whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw', class: 'input mono' });
      const payload = textarea({ rows: 4 });
      payload.value = '{"type":"order.created","data":{"id":"ord_123","amount":4200}}';
      const tamper = checkbox('Tamper with the body before verifying', false);
      const signed = h('div');
      const verifyOut = h('div');
      let last = null;
      async function sign() {
        replace(signed, spinner('POST /webhooks/sign'));
        const r = await send({ method: 'POST', url: SERVER + '/webhooks/sign?scheme=' + scheme.value + '&secret=' + encodeURIComponent(secret.value), headers: [['Content-Type', 'application/json']], body: payload.value });
        if (!alive) return;
        if (!r.ok || !r.json) { replace(signed, responseView(r)); return; }
        last = r.json;
        replace(signed, h('div', { class: 'stack-sm' },
          h('span', { class: 'label' }, 'Signed content'), codeBlock(last.signed_content || last.body, null, 'short'),
          h('span', { class: 'label' }, 'Headers to send'), headersTable(Object.entries(last.headers || {}), { noHighlight: true }),
          h('div', { class: 'row' }, h('button', { class: 'btn primary', type: 'button', onclick: verify }, icon('check'), 'Verify at /webhooks/verify'), tamper.el)));
      }
      async function verify() {
        if (!last) return;
        const headers = Object.entries(last.headers || {}).map(([k, v]) => [k, v]);
        headers.push(['X-Webhook-Secret', secret.value]);
        const body = tamper.input.checked ? last.body.replace(/\d/, (d) => String((Number(d) + 1) % 10)) : last.body;
        replace(verifyOut, spinner('Verifying'));
        const path = scheme.value === 'standard' ? '/webhooks/verify' : '/webhooks/verify/' + scheme.value;
        const r = await send({ method: 'POST', url: SERVER + path, headers, body });
        if (!alive) return;
        replace(verifyOut, h('div', { class: 'stack-sm' },
          r.ok ? notice('ok', 'Signature valid.') : notice('err', `Rejected: ${(r.json && (r.json.error || r.json.reason)) || r.status}${tamper.input.checked ? ' (the body was tampered with, as expected)' : ''}`),
          responseView(r)));
      }
      return h('div', { class: 'card-body stack' },
        notice('', 'Produces a signed webhook with the server\'s signing endpoint, then verifies it. Replay the same headers and body through the gateway to test its webhook signature validation.'),
        h('div', { class: 'grid grid-2' }, field('Scheme', scheme), field('Secret', secret, 'Standard Webhooks secrets are whsec_<base64>; the demo secret is preset')),
        field('Payload', payload),
        h('div', { class: 'row' }, h('button', { class: 'btn primary', type: 'button', onclick: sign }, icon('shield'), 'Sign')),
        signed, verifyOut);
    }

    return () => { alive = false; clearInterval(countdown); };
  },
};

