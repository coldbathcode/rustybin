// Overview: who is answering, is it healthy, how busy is it, and the base
// URLs a presenter pastes into a gateway configuration.
import { h, replace, card, kvgrid, notice, copyButton, fmtDuration, badge, icon, spinner } from '../lib/dom.js';
import { SERVER, getJson, send } from '../lib/http.js';
import { jsonView } from '../lib/format.js';

function baseUrls(cfg) {
  const loc = new URL(SERVER);
  const host = loc.hostname.includes(':') ? `[${loc.hostname}]` : loc.hostname;
  const prefix = loc.pathname.replace(/\/$/, '');
  const http = SERVER;
  const httpsPort = cfg && cfg.https_port ? cfg.https_port : 443;
  const grpcPort = cfg && cfg.grpc_port ? cfg.grpc_port : 50051;
  const https = loc.protocol === 'https:' ? SERVER : `https://${host}${httpsPort === 443 ? '' : ':' + httpsPort}${prefix}`;
  const ws = SERVER.replace(/^http/, 'ws');
  return [
    ['HTTP', http, 'Base URL of every endpoint'],
    ['HTTPS', https, loc.protocol === 'https:' ? 'This page is served over HTTPS' : 'Self-signed demo certificate (also used by mTLS)'],
    ['gRPC', `${host}:${grpcPort}`, 'Plaintext gRPC with reflection: rustybin.echo.v1.EchoService'],
    ['WebSocket', ws + '/ws', 'Echo WebSocket (/ws/time streams the clock)'],
    ['MCP', http + '/mcp', 'Streamable HTTP, 2026-07-28 stateless and 2025-11-25 sessions'],
    ['MCP (OAuth)', http + '/mcp/protected', 'Bearer token from the built-in IdP'],
    ['A2A', http + '/.well-known/agent-card.json', 'Agent card; directory at /a2a'],
    ['OpenAI', http + '/ai/openai/v1', 'Chat completions, Responses, embeddings, models'],
    ['Anthropic', http + '/ai/anthropic', 'Messages API (/v1/messages)'],
    ['Gemini', http + '/ai/gemini', 'generateContent / streamGenerateContent'],
    ['Ollama', http + '/ai/ollama', '/api/chat, /api/generate (NDJSON)'],
    ['Bedrock', http + '/ai/bedrock', 'Converse, ConverseStream, InvokeModel'],
    ['Azure OpenAI', http + '/ai/azure', '/openai/deployments/{deployment}/...'],
    ['OIDC issuer', http + '/.well-known/openid-configuration', 'Built-in IdP: rustybin / secret'],
    ['GraphQL', http + '/graphql', 'Queries, mutations, subscriptions over /graphql/ws'],
    ['Request bin', http + '/bin', 'POST to create a bin'],
  ];
}

function tile(label, value, sub, extra) {
  return h('div', { class: 'card tile' }, h('span', { class: 'label' }, label), h('div', { class: 'value' }, value), sub ? h('div', { class: 'sub' }, sub) : null, extra || null);
}

export default {
  id: 'overview',
  mount(root) {
    let timer = null;
    let tick = null;
    let alive = true;
    const tilesEl = h('div', { class: 'tiles' }, spinner('Loading status'));
    const identityEl = h('div', null, spinner('Asking /identity'));
    const urlsEl = h('div');
    const configEl = h('div', null, spinner());
    const errEl = h('div');

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'Overview'),
        h('p', null, 'This instance is the upstream your gateway talks to. Copy a base URL into the gateway configuration, then use Live traffic to see exactly what the gateway sent.')),
      h('div', { class: 'row' },
        h('a', { class: 'btn', href: '#/traffic' }, icon('traffic'), 'Live traffic'),
        h('a', { class: 'btn primary', href: '#/explorer' }, icon('explorer'), 'Try an endpoint'))));
    root.appendChild(h('div', { class: 'stack' }, errEl, tilesEl,
      h('div', { class: 'grid grid-2' },
        card('Base URLs', { flush: true, hint: 'derived from this page' }, urlsEl),
        h('div', { class: 'stack' },
          card('What Rustybin sees of this browser', { hint: 'GET /identity' }, identityEl),
          card('Quick links', null, h('div', { class: 'pill-list' },
            ...[['Endpoint list', '/'], ['OpenAPI docs', '/docs'], ['openapi.json', '/openapi.json'], ['Postman', '/export/postman.json'],
              ['Insomnia', '/export/insomnia.json'], ['Bruno', '/export/bruno.json'], ['k6 script', '/export/k6.js'], ['HAR', '/export/har.json'],
              ['curl script', '/export/curl.sh'], ['Agent card', '/.well-known/agent-card.json'], ['JWKS', '/oauth/jwks']]
              .map(([label, path]) => h('a', { class: 'btn sm', href: SERVER + path, target: '_blank', rel: 'noopener' }, label, icon('external'))))),
          card('Configuration', { hint: 'GET /_rustybin/config (no secrets)' }, configEl)))));

    replace(urlsEl, ...baseUrls(null).map(row));

    function row([label, url, hint]) {
      return h('div', { class: 'url-row' },
        h('div', { class: 'label-col' }, h('strong', { class: 'small' }, label), h('div', { class: 'tiny muted' }, hint)),
        h('code', null, url),
        copyButton(url, { icon: true, title: 'Copy ' + label + ' URL' }));
    }

    let uptimeBase = 0;
    let uptimeAt = 0;
    async function loadStatus() {
      try {
        const st = await getJson('/_rustybin/status', { session: false });
        if (!alive) return;
        replace(errEl);
        uptimeBase = st.uptime_seconds;
        uptimeAt = performance.now();
        const uptime = h('span', { id: 'uptime' }, fmtDuration(st.uptime_seconds));
        replace(tilesEl,
          tile('Health', h('span', { class: 'row', style: { gap: '8px' } }, h('span', { class: 'dot ' + (st.healthy ? 'ok' : 'err'), style: { width: '12px', height: '12px' } }), st.healthy ? 'Healthy' : 'Unhealthy'),
            st.healthy ? 'GET /health answers 200' : 'GET /health answers 503',
            h('a', { class: 'small', href: '#/chaos' }, 'Toggle in Chaos and health')),
          tile('Uptime', uptime, 'since ' + new Date(st.started_at).toLocaleString()),
          tile('Requests captured', st.inspector.total_captured.toLocaleString(), `${st.inspector.stored} of ${st.inspector.capacity} kept in the inspector`),
          tile('Instance', h('span', { class: 'mono', style: { fontSize: '16px', wordBreak: 'break-all' } }, st.instance_id),
            `${st.hostname} | v${st.version}`, h('div', { class: 'row', style: { marginTop: '6px', gap: '6px' } },
              st.public_mode ? badge('public mode', 'warn') : badge('private mode', 'ok'),
              st.admin_token_configured ? badge('admin token set', 'info') : null)));
      } catch (e) {
        if (!alive) return;
        replace(errEl, notice('err', 'Cannot read /_rustybin/status: ' + e.message));
        replace(tilesEl);
      }
    }

    async function loadConfig() {
      try {
        const cfg = await getJson('/_rustybin/config', { session: false });
        if (!alive) return;
        replace(urlsEl, ...baseUrls(cfg).map(row));
        replace(configEl, kvgrid([
          ['Ports', `http ${cfg.http_port}, https ${cfg.https_port}, grpc ${cfg.grpc_port}`],
          ['Public mode', String(cfg.public_mode)],
          ['Trust X-Forwarded-*', String(cfg.trust_forward)],
          ['CORS origins', (cfg.cors_allow_origins || []).join(', ') || 'disabled'],
          ['Body limit', cfg.body_limit + ' bytes'],
          ['Request timeout', cfg.request_timeout_secs + ' s'],
          ['Max injected delay', cfg.max_delay_ms + ' ms'],
          ['Inspector capacity', String(cfg.inspector_capacity)],
          ['Admin token', cfg.admin_token_configured ? 'configured' : 'not set'],
          ['Log level', cfg.log_level],
        ]));
      } catch (e) {
        if (alive) replace(configEl, notice('err', 'Cannot read /_rustybin/config: ' + e.message));
      }
    }

    async function loadIdentity() {
      const r = await send({ url: SERVER + '/identity', timeout: 10000, headers: [['Accept', 'application/json']] });
      if (!alive) return;
      if (r.error || !r.json) {
        replace(identityEl, notice('err', r.error || `GET /identity answered ${r.status}`));
        return;
      }
      const req = r.json.request || {};
      replace(identityEl, h('div', { class: 'stack-sm' },
        kvgrid([
          ['Client IP', req.remote_ip],
          ['TCP peer', req.peer_ip],
          ['X-Forwarded-For', req.forwarded_for],
          ['Via', req.via],
          ['Host', req.host],
          ['Scheme', req.scheme],
          ['Listener', r.json.port && r.json.port.listener ? `${r.json.port.listener.scheme} :${r.json.port.listener.port}` : undefined],
          ['Built with', r.json.environment ? `${r.json.environment.rust_version} (${r.json.environment.profile})` : undefined],
        ]),
        h('details', { class: 'disclosure' }, h('summary', null, 'Full /identity response'), jsonView(r.json, 'short'))));
    }

    loadStatus();
    loadConfig();
    loadIdentity();
    timer = setInterval(loadStatus, 5000);
    tick = setInterval(() => {
      const el = document.getElementById('uptime');
      if (el && uptimeAt) el.textContent = fmtDuration(uptimeBase + (performance.now() - uptimeAt) / 1000);
    }, 1000);
    return () => {
      alive = false;
      clearInterval(timer);
      clearInterval(tick);
    };
  },
};
