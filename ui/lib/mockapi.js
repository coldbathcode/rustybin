// Mock APIs: calls to the management API (/_rustybin/mocks*), name rules,
// permissions (read-only control tokens) and small display helpers shared by
// the list, the create wizard and the detail view.
import { h, toast } from './dom.js';
import { SERVER, send, getJson } from './http.js';
import * as auth from './auth.js';
import * as settings from './settings.js';

export const API = '/_rustybin/mocks';
export const NAME_RE = /^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$/;
export const RESERVED = ['import', 'export', 'apply', 'reload', 'visit', 'visits'];

/**
 * Call the management API. `path` is relative to /_rustybin/mocks; objects are
 * sent as JSON, strings as is. Mutations carry the admin credentials.
 */
export function api(method, path, body, opts = {}) {
  const headers = [];
  let payload;
  if (body !== undefined) {
    if (typeof body === 'string') {
      payload = body;
      headers.push(['Content-Type', opts.contentType || 'text/plain']);
    } else {
      payload = JSON.stringify(body);
      headers.push(['Content-Type', 'application/json']);
    }
  }
  return send({ method, url: SERVER + API + path, headers, body: payload, session: false, admin: method !== 'GET', timeout: opts.timeout || 20000, signal: opts.signal });
}

/** GET JSON from the management API (throws an Error with a readable message). */
export function getMock(path) {
  return getJson(API + path, { session: false });
}

/** Readable error of a failed call. */
export function errorText(r, what) {
  if (r.error) return r.error;
  const j = r.json || {};
  // Self-service answers carry a code (rustybin_...) and a readable message.
  if (/^rustybin_/.test(j.error || '') && j.message) return j.message;
  const msg = r.json && (r.json.error || r.json.detail || r.json.message);
  if (r.status === 401 || r.status === 403) {
    return (msg || 'Not allowed') + '. Changes need the admin token (console settings) or a control token with the admin scope.';
  }
  return msg || `${what || 'The request'} answered ${r.status}`;
}

/** Toast the outcome of a mutation; returns true on success. */
export function report(r, okText, what) {
  if (r.ok) {
    if (okText) toast(okText);
    return true;
  }
  toast(errorText(r, what), 'err');
  return false;
}

/** Why the console may not change mocks, or '' when it may. */
export function readOnlyReason() {
  if (auth.required()) {
    const sc = auth.scopes();
    if (sc === null || sc.includes('admin') || settings.get('adminToken')) return '';
    return 'Your control token is read-only (scope ' + sc.join(' ') + '): you can look at mocks and call them, but changing them needs a token with the admin scope.';
  }
  if (settings.instance.publicMode && !settings.instance.adminTokenConfigured) {
    return 'This instance runs in public mode without an admin token: mocks can be used but not changed from here.';
  }
  return '';
}

export function canEdit() {
  return readOnlyReason() === '';
}

/** Why the console may not change this mock (its detail), or '' when it may. */
export function mockReadOnly(detail) {
  if (detail && detail.can_manage === true) return '';
  const instance = readOnlyReason();
  if (detail && detail.can_manage === false) {
    return instance || 'This mock belongs to the instance: changing it needs the admin token (console settings).';
  }
  return instance;
}

// Where each mock answers (visitor mocks live on the mocks domain).
const bases = new Map();

/** Remember a mock's base URL (`base_url` of its detail). */
export function setMockBase(name, base) {
  if (name && base) bases.set(name, base);
}

/** Base URL of a mock without its server prefix. */
export function mockBase(name) {
  return bases.get(name) || SERVER + '/mock/' + name;
}

/** Problem with a mock name, or '' when it is valid. */
export function nameProblem(name, taken) {
  if (!name) return 'Pick a name.';
  if (name.length > 40) return 'At most 40 characters.';
  if (!NAME_RE.test(name)) return 'Use lowercase letters, digits and dashes (not at the start or end).';
  if (RESERVED.includes(name)) return `"${name}" is reserved.`;
  if (taken && taken.includes(name)) return `A mock named "${name}" already exists.`;
  return '';
}

/** Turn any title into a valid name candidate. */
export function slug(text) {
  const s = String(text || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40).replace(/-+$/, '');
  return s || 'mock';
}

/** Base URL of a mock (with the spec's server prefix unless stripped). */
export function mockUrl(name, prefix) {
  return SERVER + '/mock/' + name + (prefix || '');
}

/** "12 s ago" */
export function relTime(iso) {
  if (!iso) return 'never';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return '';
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 5) return 'just now';
  if (s < 60) return s + ' s ago';
  if (s < 3600) return Math.floor(s / 60) + ' min ago';
  if (s < 86400) return Math.floor(s / 3600) + ' h ago';
  return Math.floor(s / 86400) + ' d ago';
}

const STATE = { live: ['Live', 'ok'], off: ['Off', ''], draft: ['Not deployed', 'warn'] };

/** State pill of a mock summary: Live / Off / Not deployed, plus draft changes. */
export function statePill(m) {
  const [label, kind] = STATE[m.state] || [m.state, ''];
  return h('span', { class: 'badge ' + kind, title: m.state === 'off' ? 'Switched off: its URL answers ' + ((m.settings && m.settings.responses && m.settings.responses.disabled_status) || 503) : null },
    h('span', { class: 'dot ' + (m.state === 'live' ? 'ok' : m.state === 'draft' ? 'warn' : '') }), label);
}

export function securityLabel(sec) {
  if (!sec || !sec.enabled) return 'open';
  return { spec: 'spec schemes', api_key: 'API key', basic: 'basic auth', bearer: 'bearer token', jwt: 'JWT' }[sec.mode] || sec.mode;
}

/** Browser download of text (exports). */
export function download(filename, text, type) {
  const url = URL.createObjectURL(new Blob([text], { type: type || 'text/plain' }));
  const a = h('a', { href: url, download: filename, style: { display: 'none' } });
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Read a dropped or chosen file as text (with a size cap). */
export function readFile(file, max) {
  return new Promise((resolve, reject) => {
    if (max && file.size > max) {
      reject(new Error(`${file.name} is ${Math.round(file.size / 1024)} KB, more than the ${Math.round(max / 1024)} KB this instance accepts.`));
      return;
    }
    const fr = new FileReader();
    fr.onload = () => resolve(String(fr.result || ''));
    fr.onerror = () => reject(new Error('Cannot read ' + file.name));
    fr.readAsText(file);
  });
}

/** Group operations (rows with `tags`) by their first tag, keeping spec order. */
export function groupByTag(ops) {
  const groups = new Map();
  for (const op of ops) {
    const tag = (op.tags && op.tags[0]) || 'Other';
    if (!groups.has(tag)) groups.set(tag, []);
    groups.get(tag).push(op);
  }
  return groups;
}

/** RFC 4122 v4 UUID from crypto.getRandomValues (works outside secure contexts). */
export function uuid() {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const x = Array.from(b, (v) => v.toString(16).padStart(2, '0')).join('');
  return `${x.slice(0, 8)}-${x.slice(8, 12)}-${x.slice(12, 16)}-${x.slice(16, 20)}-${x.slice(20)}`;
}

/** Request of a sample ({method, path, query: [[k, v]], headers, content_type, body}) for send(). */
export function sampleToRequest(name, sample) {
  const q = new URLSearchParams();
  for (const [k, v] of sample.query || []) q.append(k, v);
  const qs = q.toString();
  const headers = (sample.headers || []).slice();
  let body;
  if (sample.body !== undefined && sample.body !== null) {
    body = typeof sample.body === 'string' ? sample.body : JSON.stringify(sample.body, null, 2);
    if (!headers.some(([k]) => k.toLowerCase() === 'content-type')) headers.push(['Content-Type', sample.content_type || 'application/json']);
  }
  return { method: sample.method, path: sample.path + (qs ? '?' + qs : ''), url: mockBase(name) + sample.path + (qs ? '?' + qs : ''), headers, body };
}

/** Response modes: [value, short label, explanation]. */
export const MODES = [
  ['examples', 'Spec examples', 'Examples of the spec, data generated from the schema where there are none.'],
  ['generated', 'Generated data', 'Always data generated from the response schemas (same seed, same data).'],
  ['stateful', 'Remember changes', 'Stateful CRUD: POST stores an item, GET reads it back, PUT / PATCH update, DELETE removes. Collections start from the list example.'],
  ['static', 'Custom only', 'Only custom responses and rules; empty bodies otherwise.'],
];

/** True when the mock (or one of its operations) answers from the in-memory store. */
export function isStateful(detail) {
  if (!detail || !detail.settings) return !!detail && detail.mode === 'stateful';
  if (detail.settings.responses.mode === 'stateful') return true;
  return Object.values(detail.settings.operations || {}).some((o) => o && o.mode === 'stateful');
}

/** "1 call" / "2 calls" */
export function plural(n, word, many) {
  return n + ' ' + (n === 1 ? word : many || word + 's');
}

/** File name from a Content-Disposition header, else the fallback. */
export function dispositionName(r, fallback) {
  const cd = (r && r.header && r.header('content-disposition')) || '';
  const m = /filename="?([^";]+)"?/i.exec(cd);
  return m ? m[1] : fallback;
}

/** Collection formats of GET /_rustybin/mocks/{name}/collections/{format}. */
export const COLLECTIONS = [
  ['postman', 'Postman collection', 'JSON, import into Postman or compatible clients'],
  ['hurl', 'Hurl file', 'runnable test file: hurl --test'],
  ['curl', 'curl script', 'shell script with one curl per operation'],
  ['http', '.http file', 'for editor REST clients'],
  ['k6', 'k6 script', 'load test script'],
];
