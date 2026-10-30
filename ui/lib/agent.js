// "Connect an AI agent": the `claude mcp add` command for this instance's
// management MCP server (/_rustybin/mcp) and the JSON snippet other MCP
// clients use, filled with the URL and the credential the console already
// holds (the control token, the admin token or the visit's manage token).
// The secret stays masked until the user asks to see it; the copy buttons
// copy the real value.
import { h, icon, copyButton, notice } from './dom.js';
import { codeBlock } from './format.js';
import * as auth from './auth.js';
import * as settings from './settings.js';
import * as visit from './visit.js';
import { SERVER } from './http.js';

export const MCP_PATH = '/_rustybin/mcp';

/** The credential an agent should present: {header, value, kind} or null. */
export function agentCredential() {
  const control = auth.getToken();
  if (control) return { header: 'Authorization', value: 'Bearer ' + control, secret: control, kind: 'control token' };
  const admin = settings.get('adminToken');
  if (admin) return { header: 'Authorization', value: 'Bearer ' + admin, secret: admin, kind: 'admin token' };
  const tokens = visit.tokens();
  if (tokens.length) return { header: 'X-Rustybin-Mock-Token', value: tokens[0], secret: tokens[0], kind: 'visit manage token' };
  return null;
}

function mask(text, secret) {
  if (!secret) return text;
  const shown = secret.length > 8 ? secret.slice(0, 4) + '...' + secret.slice(-2) : '...';
  return text.split(secret).join(shown);
}

function shellQuote(s) {
  return "'" + String(s).replace(/'/g, "'\\''") + "'";
}

/** The commands and snippets, with the secret in the clear. */
export function agentSnippets() {
  const url = SERVER + MCP_PATH;
  const cred = agentCredential();
  let claude = 'claude mcp add --transport http rustybin ' + shellQuote(url);
  if (cred) claude += ' --header ' + shellQuote(cred.header + ': ' + cred.value);
  const json = { mcpServers: { rustybin: { type: 'http', url } } };
  if (cred) json.mcpServers.rustybin.headers = { [cred.header]: cred.value };
  return { url, cred, claude, json: JSON.stringify(json, null, 2) };
}

/**
 * The panel. opts.compact renders a disclosure (closed by default) for a
 * view; without it the content is rendered open (the settings dialog).
 */
export function agentPanel(opts = {}) {
  const { url, cred, claude, json } = agentSnippets();
  let revealed = false;
  const claudeEl = h('pre', { class: 'code', tabindex: '0' });
  const jsonEl = h('pre', { class: 'code', tabindex: '0' });
  const render = () => {
    claudeEl.textContent = revealed ? claude : mask(claude, cred && cred.secret);
    jsonEl.textContent = revealed ? json : mask(json, cred && cred.secret);
  };
  render();
  const reveal = cred ? h('button', { class: 'btn sm', type: 'button', onclick: () => { revealed = !revealed; reveal.textContent = revealed ? 'Hide the token' : 'Show the token'; render(); } }, 'Show the token') : null;
  const credNote = cred
    ? h('p', { class: 'small muted', style: { margin: 0 } }, 'Uses the ', cred.kind, ' this console holds, sent as ', h('code', null, cred.header), '. The agent gets the same rights as you have here.')
    : h('p', { class: 'small muted', style: { margin: 0 } }, settings.instance.publicMode
      ? 'No credential held: on this public instance an agent can read and call mocks; creating one starts a self-service visit when the instance allows it.'
      : 'This instance needs no credential: the control plane is open.');
  const body = h('div', { class: 'stack-sm' },
    h('p', { style: { margin: 0 } }, 'Let an agent such as Claude Code configure this Rustybin: create and deploy mocks, change responses and security, send test requests and read what your gateway sent. Destructive tools ask for confirmation.'),
    h('div', { class: 'row', style: { alignItems: 'center' } }, h('strong', { class: 'small' }, 'Claude Code'), h('span', { class: 'grow' }), copyButton(() => claude, { label: 'Copy command' })),
    claudeEl,
    h('div', { class: 'row', style: { alignItems: 'center' } }, h('strong', { class: 'small' }, 'Other MCP clients (mcpServers JSON)'), h('span', { class: 'grow' }), copyButton(() => json, { label: 'Copy JSON' })),
    jsonEl,
    h('div', { class: 'row', style: { alignItems: 'center', gap: '8px' } }, credNote, h('span', { class: 'grow' }), reveal),
    h('p', { class: 'tiny muted', style: { margin: 0 } }, 'Endpoint: ', h('code', null, url), '. Clients that only speak stdio: ', h('code', null, 'rustybin mcp-stdio --server ' + url), '. See the documentation page "Configure Rustybin from an AI agent".'));
  if (!opts.compact) return body;
  return h('details', { class: 'disclosure' },
    h('summary', null, icon('terminal'), ' Connect an AI agent'),
    h('div', { style: { paddingTop: '8px' } }, body));
}

/** A one-line notice pointing at the panel (used where space is short). */
export function agentHint() {
  return notice('info', icon('terminal'), h('span', null, 'An AI agent can manage these mocks through ', h('code', null, MCP_PATH), ': open AI agents for the connection command.'));
}
