// Body and header rendering: JSON highlighting, XML and form pretty printing,
// gateway header detection and line diffs.
import { h, copyButton } from './dom.js';

const MAX_RENDER = 400 * 1024;

/** Highlighted JSON as a <pre>. `value` is a parsed value or a JSON string. */
export function jsonView(value, cls) {
  let text;
  if (typeof value === 'string') {
    try { text = JSON.stringify(JSON.parse(value), null, 2); } catch { text = value; }
  } else {
    text = JSON.stringify(value, null, 2);
  }
  if (text === undefined) text = String(value);
  return codeBlock(text, 'json', cls);
}

function highlightJson(pre, text) {
  if (text.length > MAX_RENDER) { pre.textContent = text; return; }
  const re = /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false)\b|\b(null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) pre.appendChild(document.createTextNode(text.slice(last, m.index)));
    let cls;
    if (m[1]) cls = m[2] ? 'j-key' : 'j-str';
    else if (m[3]) cls = 'j-bool';
    else if (m[4]) cls = 'j-null';
    else cls = 'j-num';
    const span = document.createElement('span');
    span.className = cls;
    span.textContent = m[1] ? m[1] : m[0];
    pre.appendChild(span);
    if (m[1] && m[2]) pre.appendChild(document.createTextNode(m[2]));
    last = re.lastIndex;
  }
  if (last < text.length) pre.appendChild(document.createTextNode(text.slice(last)));
}

function highlightXml(pre, text) {
  if (text.length > MAX_RENDER) { pre.textContent = text; return; }
  const re = /(<\/?[\w:.-]+)|([\w:.-]+=)("[^"]*"|'[^']*')|(\/?>)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) pre.appendChild(document.createTextNode(text.slice(last, m.index)));
    if (m[1] || m[4]) {
      const sp = document.createElement('span'); sp.className = 'x-tag'; sp.textContent = m[1] || m[4]; pre.appendChild(sp);
    } else {
      const a = document.createElement('span'); a.className = 'x-attr'; a.textContent = m[2]; pre.appendChild(a);
      const v = document.createElement('span'); v.className = 'j-str'; v.textContent = m[3]; pre.appendChild(v);
    }
    last = re.lastIndex;
  }
  if (last < text.length) pre.appendChild(document.createTextNode(text.slice(last)));
}

/** <pre class="code"> with optional highlighting ('json' | 'xml' | null). */
export function codeBlock(text, lang, cls) {
  const pre = h('pre', { class: 'code ' + (cls || ''), tabindex: '0' });
  if (lang === 'json') highlightJson(pre, text);
  else if (lang === 'xml') highlightXml(pre, text);
  else pre.textContent = text;
  return pre;
}

/** Code block with a copy button in the corner. */
export function copyableCode(text, lang, cls) {
  return h('div', { class: 'code-wrap' }, codeBlock(text, lang, cls), h('span', { class: 'copy-fab' }, copyButton(text, { icon: true, title: 'Copy' })));
}

export function prettyXml(xml) {
  const tokens = xml.replace(/>\s+</g, '><').replace(/</g, '\n<').split('\n').filter((t) => t.trim());
  let depth = 0;
  const out = [];
  for (const t of tokens) {
    if (/^<\//.test(t)) depth = Math.max(0, depth - 1);
    out.push('  '.repeat(depth) + t);
    if (/^<[^!?/][^>]*[^/]>[^<]*$/.test(t) && !/<\/[^>]+>$/.test(t)) depth++;
  }
  return out.join('\n');
}

export function detectKind(text, contentType) {
  const ct = (contentType || '').toLowerCase();
  const t = (text || '').trim();
  if (ct.includes('json') || /^[{[]/.test(t)) {
    try { JSON.parse(t); return 'json'; } catch { if (ct.includes('json')) return 'text'; }
  }
  if (ct.includes('x-www-form-urlencoded')) return 'form';
  if (ct.includes('xml') || /^<\?xml|^<[\w:]+[\s>]/.test(t)) return 'xml';
  if (ct.includes('event-stream') || /^(data|event):/m.test(t)) return 'sse';
  return 'text';
}

/** Render a body with the right viewer (JSON, XML, form table, SSE, text). */
export function bodyView(text, contentType, opts = {}) {
  if (text === null || text === undefined || text === '') return h('p', { class: 'muted small' }, opts.emptyText || 'No body');
  const kind = detectKind(text, contentType);
  if (kind === 'json') {
    let pretty;
    try { pretty = JSON.stringify(JSON.parse(text), null, 2); } catch { pretty = text; }
    return codeBlock(pretty, 'json', opts.cls);
  }
  if (kind === 'xml') return codeBlock(prettyXml(text), 'xml', opts.cls);
  if (kind === 'form') {
    const rows = [];
    for (const [k, v] of new URLSearchParams(text)) rows.push({ name: k, value: v });
    return h('div', { class: 'stack-sm' }, headersTable(rows, { noHighlight: true, keyLabel: 'Field' }), codeBlock(text, null, 'short'));
  }
  return codeBlock(text, null, opts.cls);
}

// Headers commonly added or rewritten by API gateways and proxies.
const GATEWAY_EXACT = new Set([
  'via', 'forwarded', 'x-real-ip', 'x-request-id', 'x-correlation-id', 'request-id', 'traceparent', 'tracestate',
  'b3', 'x-b3-traceid', 'x-b3-spanid', 'x-b3-parentspanid', 'x-b3-sampled', 'x-cloud-trace-context', 'x-trace-id',
  'authorization', 'proxy-authorization', 'x-api-key', 'api-key', 'apikey', 'x-client-id', 'x-userinfo', 'x-jwt-claims',
  'x-credential-identifier', 'x-anonymous-consumer', 'x-authenticated-scope', 'x-authenticated-userid', 'x-ratelimit-limit',
  'x-ratelimit-remaining', 'x-ratelimit-reset', 'x-original-uri', 'x-original-url', 'x-original-method', 'x-rewrite-url',
  'x-gateway', 'x-proxy-id', 'x-tenant-id', 'x-apim-request-id', 'cf-ray', 'cf-connecting-ip', 'true-client-ip',
  'fastly-client-ip', 'x-azure-ref', 'x-ms-request-id', 'x-goog-api-key', 'x-client-cert', 'ssl-client-cert', 'x-ssl-client-cert',
  'x-forwarded-client-cert',
]);
const GATEWAY_PREFIXES = ['x-forwarded-', 'x-consumer-', 'x-kong-', 'x-envoy-', 'x-amzn-', 'x-amz-', 'x-apigw-', 'x-apigateway-',
  'x-tyk-', 'x-gravitee-', 'x-ms-apim-', 'x-mulesoft-', 'x-3scale-', 'x-istio-', 'x-datadog-', 'x-ot-', 'x-ratelimit-', 'ratelimit-',
  'x-auth-', 'x-user-', 'x-oidc-', 'x-jwt-', 'x-client-', 'x-ssl-', 'x-gw-', 'x-api-', 'x-original-'];

export function isGatewayHeader(name) {
  const n = String(name).toLowerCase();
  return GATEWAY_EXACT.has(n) || GATEWAY_PREFIXES.some((p) => n.startsWith(p));
}

/** Headers table; rows: [{name, value}] or [[name, value]]. */
export function headersTable(rows, opts = {}) {
  const list = (rows || []).map((r) => (Array.isArray(r) ? { name: r[0], value: r[1] } : r));
  const tbody = h('tbody');
  for (const r of list) {
    const gw = !opts.noHighlight && isGatewayHeader(r.name);
    tbody.appendChild(h('tr', { class: gw ? 'hl' : null },
      h('td', { class: 'k' }, r.name, gw ? h('span', { class: 'tag-gw', title: 'Commonly added or rewritten by gateways and proxies' }, 'gateway') : null),
      h('td', { class: 'v' }, r.value)));
  }
  if (!list.length) tbody.appendChild(h('tr', null, h('td', { colspan: '2', class: 'muted' }, opts.emptyText || 'No headers')));
  return h('table', { class: 'table' }, h('thead', null, h('tr', null, h('th', null, opts.keyLabel || 'Name'), h('th', null, 'Value'))), tbody);
}

// ── Diff ───────────────────────────────────────────────────────────

/** Line diff (LCS). Returns [{op: ' '|'+'|'-', line}]. */
export function diffLines(a, b) {
  const A = a.split('\n'), B = b.split('\n');
  if (A.length * B.length > 4_000_000) {
    return [...A.map((line) => ({ op: '-', line })), ...B.map((line) => ({ op: '+', line }))];
  }
  const n = A.length, m = B.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = A[i] === B[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out = [];
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (A[i] === B[j]) { out.push({ op: ' ', line: A[i] }); i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) { out.push({ op: '-', line: A[i++] }); }
    else { out.push({ op: '+', line: B[j++] }); }
  }
  while (i < n) out.push({ op: '-', line: A[i++] });
  while (j < m) out.push({ op: '+', line: B[j++] });
  return out;
}

export function diffView(a, b) {
  const pre = h('pre', { class: 'code tall', tabindex: '0' });
  for (const d of diffLines(a, b)) {
    const cls = d.op === '+' ? 'd-add' : d.op === '-' ? 'd-del' : 'd-ctx';
    pre.appendChild(h('span', { class: cls }, d.op + ' ' + d.line));
  }
  return pre;
}

/** Normalise a body for diffing (pretty JSON when possible). */
export function normaliseBody(text) {
  if (!text) return '';
  try { return JSON.stringify(JSON.parse(text), null, 2); } catch { return text; }
}
