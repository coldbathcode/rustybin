// A2A client: agent directory and cards, send (blocking or streaming) in
// v1.0 or v0.3 JSON-RPC, a task timeline with state badges and artifacts,
// input-required / auth-required continuation, cancel, and the push
// notification sink.
import { h, replace, clear, card, field, input, notice, empty, icon, badge, fmtTime, segmented, spinner, toast, checkbox, randomId, copyText } from '../lib/dom.js';
import { jsonView, codeBlock } from '../lib/format.js';
import { send, sseParser, readText, toCurl } from '../lib/http.js';
import { targetPicker } from '../lib/widgets.js';
import { base64UrlText } from '../lib/crypto.js';
import * as settings from '../lib/settings.js';

const STATE_KIND = {
  submitted: 'info', working: 'info', completed: 'ok', failed: 'err', canceled: 'warn', rejected: 'err',
  'input-required': 'violet', 'auth-required': 'warn', unknown: '',
};
const TERMINAL = new Set(['completed', 'failed', 'canceled', 'rejected']);

export function normState(s) {
  return String(s || 'unknown').replace(/^TASK_STATE_/i, '').toLowerCase().replace(/_/g, '-').replace('cancelled', 'canceled');
}

function normPart(p) {
  if (!p) return { kind: 'unknown', raw: p };
  if (p.kind === 'text' || (p.text !== undefined && !p.kind)) return { kind: 'text', text: p.text };
  if (p.kind === 'data' || (p.data !== undefined && !p.kind)) return { kind: 'data', data: p.data };
  if (p.kind === 'file') return { kind: 'file', url: p.file && p.file.uri, bytes: p.file && p.file.bytes, mediaType: p.file && p.file.mimeType, name: p.file && p.file.name };
  if (p.url !== undefined || p.raw !== undefined) return { kind: 'file', url: p.url, bytes: p.raw, mediaType: p.mediaType, name: p.filename };
  return { kind: 'unknown', raw: p };
}

/** {type: task|status|artifact|message, ...} from a v1.0 StreamResponse or a v0.3 result. */
export function normEvent(r) {
  if (!r || typeof r !== 'object') return null;
  if (r.task || r.kind === 'task') return { type: 'task', task: r.task || r };
  if (r.statusUpdate || r.kind === 'status-update') return { type: 'status', ev: r.statusUpdate || r };
  if (r.artifactUpdate || r.kind === 'artifact-update') return { type: 'artifact', ev: r.artifactUpdate || r };
  if (r.message || r.kind === 'message') return { type: 'message', message: r.message || r };
  if (r.id && r.status) return { type: 'task', task: r };
  return null;
}

function textOf(message) {
  return ((message && message.parts) || []).map(normPart).filter((p) => p.kind === 'text').map((p) => p.text).join('');
}

function stateBadge(state) {
  return badge(state, STATE_KIND[state] || '');
}

export default {
  id: 'a2a',
  mount(root) {
    let alive = true;
    let ctrl = null;
    let sinkTimer = null;
    const st = settings.recall('a2a.state', { agent: 'travel-planner', version: '1.0', mode: 'stream', token: '', push: false, sinkId: randomId('console-', 4), delay: 400, task: null, events: [], artifacts: {} });
    settings.remember('a2a.state', st);
    const target = targetPicker('a2a');
    let agents = [];

    const dirEl = h('div', null, spinner('Loading agents'));
    const cardEl = h('div');
    const taskEl = h('div');
    const timelineEl = h('div', { class: 'timeline' });
    const artifactsEl = h('div', { class: 'stack-sm' });
    const sinkEl = h('div');
    const textIn = input({ placeholder: 'Message the agent', 'aria-label': 'Message' });
    const sendBtn = h('button', { class: 'btn primary', type: 'button' }, icon('send'), 'Send');
    const stopBtn = h('button', { class: 'btn', type: 'button', hidden: true }, icon('stop'), 'Stop stream');
    const versionSeg = segmented([['1.0', 'v1.0 (A2A-Version: 1.0)'], ['0.3', 'v0.3']], st.version, (v) => { st.version = v; }, 'Protocol version');
    const modeSeg = segmented([['stream', 'Stream'], ['send', 'Send (blocking)']], st.mode, (v) => { st.mode = v; }, 'Send mode');
    const tokenIn = input({ value: st.token, class: 'input mono', placeholder: 'Bearer token (secure agent)' });
    tokenIn.addEventListener('input', () => { st.token = tokenIn.value.trim(); });
    const delayIn = input({ type: 'number', min: '0', max: '5000', value: String(st.delay), class: 'input mono' });
    delayIn.addEventListener('input', () => { st.delay = Number(delayIn.value) || 0; });
    const pushCb = checkbox('Push notifications to the built-in sink', st.push, (v) => { st.push = v; renderSink(); });

    textIn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); sendMessage(); } });
    sendBtn.addEventListener('click', () => sendMessage());
    stopBtn.addEventListener('click', () => { if (ctrl) ctrl.abort(); });

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'A2A client'),
        h('p', null, 'Talk to the mock Agent2Agent agents: discover their cards, send messages, and follow tasks as they stream status updates and artifacts. Route through the gateway to demo agent routing, auth and observability.')),
      h('div', { class: 'row' }, target.el)));
    root.appendChild(h('div', { class: 'grid grid-sidebar' },
      h('div', { class: 'stack' }, card('Agents', { flush: true, hint: 'GET /a2a', actions: h('button', { class: 'btn sm ghost icon', type: 'button', 'aria-label': 'Reload agents', onclick: loadDirectory }, icon('refresh')) }, dirEl), cardEl),
      h('div', { class: 'stack' },
        card('Message', null, h('div', { class: 'stack-sm' },
          h('div', { class: 'row' }, versionSeg.el, modeSeg.el),
          h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, textIn), sendBtn, stopBtn),
          h('div', { class: 'grid grid-3' },
            field('Step delay (ms)', delayIn, 'metadata.stepDelayMs (travel planner pace)'),
            h('div', { class: 'field' }, h('span', null, 'Bearer token'), h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, tokenIn), h('button', { class: 'btn sm', type: 'button', onclick: getToken }, 'Get token'))),
            h('div', { class: 'field' }, h('span', null, 'Push'), pushCb.el)))),
        taskEl,
        h('div', { class: 'grid grid-2' },
          card('Timeline', { actions: h('button', { class: 'btn sm', type: 'button', onclick: () => { st.events = []; st.artifacts = {}; st.task = null; renderTask(); } }, icon('trash'), 'Clear') }, timelineEl),
          h('div', { class: 'stack' }, card('Artifacts', null, artifactsEl), sinkEl)))));

    // ── Directory and cards ──
    async function loadDirectory() {
      replace(dirEl, h('div', { class: 'card-body' }, spinner('GET /a2a')));
      const r = await send({ url: target.base() + '/a2a', timeout: 10000 });
      if (!alive) return;
      if (!r.json || !r.json.agents) { replace(dirEl, h('div', { class: 'card-body' }, notice('err', r.error || `GET /a2a answered ${r.status}`))); return; }
      agents = r.json.agents;
      renderDirectory();
      pickAgent(st.agent && agents.some((a) => a.id === st.agent) ? st.agent : agents[0].id);
    }

    function renderDirectory() {
      replace(dirEl, h('div', { class: 'list' }, ...agents.map((a) => h('button', { class: 'list-item', type: 'button', 'aria-selected': String(a.id === st.agent), style: { gridTemplateColumns: 'minmax(0,1fr) auto' }, onclick: () => pickAgent(a.id) },
        h('span', { style: { minWidth: 0 } }, h('div', { style: { fontWeight: 600 } }, a.name), h('div', { class: 'tiny muted ellipsis' }, a.description)),
        a.requiresAuth ? badge('auth', 'warn') : h('span', { class: 'mono tiny muted' }, a.id)))));
    }

    async function pickAgent(id) {
      st.agent = id;
      renderDirectory();
      replace(cardEl, card('Agent card', null, spinner('Loading the card')));
      const url = target.base() + '/a2a/' + id + '/.well-known/agent-card.json';
      const r = await send({ url, timeout: 10000, headers: st.version === '1.0' ? [['A2A-Version', '1.0']] : [] });
      if (!alive || st.agent !== id) return;
      if (!r.json) { replace(cardEl, card('Agent card', null, notice('err', r.error || `answered ${r.status}`))); return; }
      const c = r.json;
      const skills = c.skills || [];
      const examples = skills.flatMap((s) => s.examples || []).slice(0, 6);
      if (!textIn.value || !textIn.dataset.touched) textIn.value = examples[0] || 'hello agent';
      replace(cardEl, card(c.name || id, { hint: 'v' + (c.version || '') }, h('div', { class: 'stack-sm' },
        h('p', { class: 'small' }, c.description),
        h('div', { class: 'pill-list' }, c.capabilities && c.capabilities.streaming ? badge('streaming', 'info') : null, c.capabilities && c.capabilities.pushNotifications ? badge('push', 'info') : null,
          c.securitySchemes || c.security || c.securityRequirements ? badge('secured', 'warn') : null, (c.supportedInterfaces || []).length ? badge(`${c.supportedInterfaces.length} interfaces`, 'outline') : null),
        ...skills.map((s) => h('div', { class: 'artifact' }, h('strong', { class: 'small' }, s.name), h('div', { class: 'tiny muted' }, s.description))),
        examples.length ? h('div', { class: 'stack-sm' }, h('span', { class: 'label' }, 'Try'), h('div', { class: 'pill-list' }, ...examples.map((ex) => h('button', { class: 'btn sm', type: 'button', onclick: () => { textIn.value = ex; sendMessage(); } }, ex)))) : null,
        h('details', { class: 'disclosure' }, h('summary', null, 'Raw agent card'), jsonView(c, 'tall')))));
    }
    textIn.addEventListener('input', () => { textIn.dataset.touched = '1'; });

    // ── Sending ──
    function buildMessage(text, continuation) {
      const v1 = st.version === '1.0';
      const msg = v1
        ? { messageId: randomId('msg-', 8), role: 'ROLE_USER', parts: [{ text }] }
        : { kind: 'message', messageId: randomId('msg-', 8), role: 'user', parts: [{ kind: 'text', text }] };
      if (continuation && st.task) { msg.taskId = st.task.id; msg.contextId = st.task.contextId; }
      if (st.delay !== undefined) msg.metadata = { stepDelayMs: Math.max(0, Math.min(5000, Number(st.delay) || 0)) };
      return msg;
    }

    function rpcHeaders() {
      const hs = [['Content-Type', 'application/json']];
      if (st.version === '1.0') hs.push(['A2A-Version', '1.0']);
      if (st.token) hs.push(['Authorization', 'Bearer ' + st.token]);
      return hs;
    }

    async function sendMessage(forceContinuation) {
      const text = textIn.value.trim();
      if (!text || ctrl) return;
      const state = st.task ? normState(st.task.status && st.task.status.state) : null;
      const continuation = forceContinuation || (st.task && (state === 'input-required' || state === 'auth-required'));
      if (!continuation) { st.events = []; st.artifacts = {}; st.task = null; }
      const v1 = st.version === '1.0';
      const stream = st.mode === 'stream';
      const params = { message: buildMessage(text, continuation) };
      if (st.push) params.configuration = { pushNotificationConfig: { url: target.base() + '/a2a/webhook-sink/' + st.sinkId, token: 'console-token' } };
      const method = v1 ? (stream ? 'SendStreamingMessage' : 'SendMessage') : (stream ? 'message/stream' : 'message/send');
      const body = { jsonrpc: '2.0', id: Date.now(), method, params };
      const url = target.base() + '/a2a/' + st.agent;
      addEvent('out', { method, text });
      ctrl = new AbortController();
      sendBtn.hidden = true;
      stopBtn.hidden = !stream;
      const req = { method: 'POST', url, headers: rpcHeaders(), body: JSON.stringify(body) };
      st.lastCurl = toCurl(Object.assign({ stream }, req));
      const r = await send(Object.assign({ stream: true, timeout: 110000, signal: ctrl.signal }, req));
      try {
        if (r.error) throw new Error(r.error);
        if (r.contentType.includes('event-stream')) {
          const parse = sseParser((ev) => {
            try { handleRpc(JSON.parse(ev.data)); } catch { /* ignore */ }
          });
          await readText(r.response, parse, ctrl.signal);
        } else {
          const text2 = await r.response.text();
          let j = null;
          try { j = JSON.parse(text2); } catch { /* not JSON */ }
          if (j) handleRpc(j, r.status);
          else addEvent('error', { message: `HTTP ${r.status}: ${text2.slice(0, 300)}` });
        }
      } catch (e) {
        if (!(ctrl && ctrl.signal.aborted)) addEvent('error', { message: e.message });
        else addEvent('note', { message: 'Stream closed by the console (the task keeps running on the server).' });
      } finally {
        ctrl = null;
        sendBtn.hidden = false;
        stopBtn.hidden = true;
        textIn.dataset.touched = '';
        if (st.task) {
          const s = normState(st.task.status && st.task.status.state);
          if (s === 'input-required') textIn.value = 'approve';
          else if (s === 'auth-required') textIn.value = 'continue with my token';
        }
        renderTask();
      }
    }

    function handleRpc(j, httpStatus) {
      if (j.error) { addEvent('error', { message: `${j.error.message} (code ${j.error.code})`, data: j.error.data, http: httpStatus }); return; }
      const ev = normEvent(j.result);
      if (!ev) { addEvent('note', { message: 'Unrecognised result', data: j.result }); return; }
      if (ev.type === 'task') {
        st.task = ev.task;
        for (const a of ev.task.artifacts || []) st.artifacts[a.artifactId] = { artifact: a, text: partsText(a.parts) };
        addEvent('task', { state: normState(ev.task.status && ev.task.status.state), text: textOf(ev.task.status && ev.task.status.message), id: ev.task.id });
      } else if (ev.type === 'status') {
        if (!st.task) st.task = { id: ev.ev.taskId, contextId: ev.ev.contextId, status: ev.ev.status };
        st.task.status = ev.ev.status;
        addEvent('status', { state: normState(ev.ev.status && ev.ev.status.state), text: textOf(ev.ev.status && ev.ev.status.message), final: ev.ev.final });
      } else if (ev.type === 'artifact') {
        const a = ev.ev.artifact || {};
        const prev = st.artifacts[a.artifactId];
        if (ev.ev.append && prev) {
          prev.artifact.parts = (prev.artifact.parts || []).concat(a.parts || []);
          prev.text += partsText(a.parts);
        } else {
          st.artifacts[a.artifactId] = { artifact: JSON.parse(JSON.stringify(a)), text: partsText(a.parts) };
        }
        st.artifacts[a.artifactId].done = !!ev.ev.lastChunk || !ev.ev.append;
        addEvent('artifact', { name: a.name || a.artifactId, append: !!ev.ev.append, last: !!ev.ev.lastChunk });
      } else if (ev.type === 'message') {
        addEvent('message', { text: textOf(ev.message) });
      }
      renderTask();
    }

    function partsText(parts) {
      return (parts || []).map(normPart).filter((p) => p.kind === 'text').map((p) => p.text).join('');
    }

    function addEvent(kind, data) {
      st.events.push({ kind, data, t: new Date().toISOString() });
      if (st.events.length > 300) st.events.shift();
      renderTimeline();
    }

    function renderTimeline() {
      clear(timelineEl);
      if (!st.events.length) { timelineEl.appendChild(empty('No task yet', 'Send a message: the travel planner streams a nice timeline, the approval agent asks for input.', 'a2a')); return; }
      for (const e of st.events) {
        const d = e.data;
        let dot = 'info', head, body = null;
        if (e.kind === 'out') { dot = ''; head = [badge('you', 'outline'), h('code', { class: 'small' }, d.method)]; body = h('div', { class: 'small' }, d.text); }
        else if (e.kind === 'task') { dot = STATE_KIND[d.state] || 'info'; head = [h('strong', null, 'Task'), stateBadge(d.state)]; body = d.text ? h('div', { class: 'small' }, d.text) : null; }
        else if (e.kind === 'status') { dot = STATE_KIND[d.state] || 'info'; head = [h('strong', null, 'Status'), stateBadge(d.state), d.final ? badge('final', 'outline') : null]; body = d.text ? h('div', { class: 'small' }, d.text) : null; }
        else if (e.kind === 'artifact') { dot = 'violet'; head = [h('strong', null, 'Artifact'), h('code', { class: 'small' }, d.name), d.append ? badge('chunk', 'outline') : null, d.last ? badge('last chunk', 'outline') : null]; }
        else if (e.kind === 'message') { dot = 'ok'; head = [h('strong', null, 'Message')]; body = h('div', { class: 'small' }, d.text); }
        else if (e.kind === 'error') { dot = 'err'; head = [h('strong', null, 'Error')]; body = h('div', { class: 'small' }, d.message, d.data ? jsonView(d.data, 'short') : null); }
        else { dot = 'warn'; head = [h('strong', null, 'Note')]; body = h('div', { class: 'small muted' }, d.message); }
        timelineEl.appendChild(h('div', { class: 'tl-item' }, h('span', { class: 'tl-dot ' + dot }),
          h('div', { class: 'tl-body' }, h('div', { class: 'tl-head' }, ...head, h('span', { class: 'tl-time' }, fmtTime(e.t))), body)));
      }
    }

    function renderTask() {
      renderTimeline();
      renderArtifacts();
      const t = st.task;
      if (!t) { replace(taskEl); return; }
      const state = normState(t.status && t.status.state);
      const prompt = state === 'input-required' ? notice('warn', h('strong', null, 'The agent needs input. '), 'Reply in the message box (for example "approve" or "deny"): the console resends with the same taskId and contextId.')
        : state === 'auth-required' ? notice('warn', h('strong', null, 'The agent needs credentials. '), 'Press "Get token" (or paste one), then send again: the message continues the same task with an Authorization header.')
          : null;
      replace(taskEl, card('Task', { actions: h('div', { class: 'row' },
        h('button', { class: 'btn sm', type: 'button', onclick: getTask }, icon('refresh'), 'Get task'),
        h('button', { class: 'btn sm danger', type: 'button', disabled: TERMINAL.has(state), onclick: cancelTask }, icon('x'), 'Cancel task'),
        st.lastCurl ? h('button', { class: 'btn sm', type: 'button', onclick: () => copyText(st.lastCurl, 'curl command copied') }, icon('terminal'), 'curl') : null) },
      h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, stateBadge(state), h('span', { class: 'mono small' }, 'task ' + t.id), h('span', { class: 'mono tiny muted' }, 'context ' + (t.contextId || ''))),
        prompt)));
    }

    function renderArtifacts() {
      clear(artifactsEl);
      const list = Object.values(st.artifacts);
      if (!list.length) { artifactsEl.appendChild(h('p', { class: 'small muted' }, 'Artifacts appear here (text streamed in chunks, JSON data, files).')); return; }
      for (const { artifact, text, done } of list) {
        const parts = (artifact.parts || []).map(normPart);
        const nonText = parts.filter((p) => p.kind !== 'text');
        artifactsEl.appendChild(h('div', { class: 'artifact' },
          h('div', { class: 'row' }, h('strong', { class: 'small' }, artifact.name || artifact.artifactId), artifact.description ? h('span', { class: 'tiny muted' }, artifact.description) : null, done === false ? h('span', { class: 'spinner' }) : null),
          text ? codeBlock(text, null, 'short') : null,
          ...nonText.map((p) => {
            if (p.kind === 'data') return jsonView(p.data, 'short');
            if (p.kind === 'file' && p.url) return h('div', null, h('a', { href: p.url, target: '_blank', rel: 'noopener' }, p.name || p.url), (p.mediaType || '').startsWith('image/') ? h('img', { src: p.url, alt: p.name || 'artifact image' }) : null);
            if (p.kind === 'file' && p.bytes) {
              let decoded = null;
              if ((p.mediaType || '').startsWith('text/')) { try { decoded = base64UrlText(p.bytes); } catch { decoded = null; } }
              return h('div', null, badge(`${p.name || 'file'} (${p.mediaType || 'bytes'})`, 'outline'), decoded ? codeBlock(decoded, null, 'short') : codeBlock(p.bytes.slice(0, 400), null, 'short'));
            }
            return jsonView(p.raw || p, 'short');
          })));
      }
    }

    async function rpc(method03, method1, params) {
      const v1 = st.version === '1.0';
      const body = { jsonrpc: '2.0', id: Date.now(), method: v1 ? method1 : method03, params };
      const r = await send({ method: 'POST', url: target.base() + '/a2a/' + st.agent, headers: rpcHeaders(), body: JSON.stringify(body), timeout: 15000 });
      if (r.error || !r.json) { toast(r.error || `answered ${r.status}`, 'err'); return null; }
      return r.json;
    }

    async function getTask() {
      if (!st.task) return;
      const j = await rpc('tasks/get', 'GetTask', { id: st.task.id });
      if (j) handleRpc(j);
    }

    async function cancelTask() {
      if (!st.task) return;
      if (ctrl) ctrl.abort();
      const j = await rpc('tasks/cancel', 'CancelTask', { id: st.task.id });
      if (j) handleRpc(j);
    }

    async function getToken() {
      const r = await send({ method: 'POST', url: target.base() + '/oauth/token', headers: [['Content-Type', 'application/x-www-form-urlencoded']], body: 'grant_type=client_credentials&client_id=rustybin&client_secret=secret&scope=openid', timeout: 10000 });
      if (r.json && r.json.access_token) {
        st.token = r.json.access_token;
        tokenIn.value = st.token;
        toast('Token from the built-in IdP (client_credentials)');
      } else {
        toast(r.error || `Token request failed: ${r.status}`, 'err');
      }
    }

    // ── Push notification sink ──
    async function pollSink() {
      if (!st.push || !alive) return;
      const r = await send({ url: target.base() + '/a2a/webhook-sink/' + st.sinkId, timeout: 8000 });
      if (!alive || !st.push) return;
      const list = (r.json && r.json.notifications) || [];
      replace(sinkBody, list.length ? h('div', { class: 'stack-sm' }, ...list.slice(-20).reverse().map((n) => h('details', { class: 'disclosure' },
        h('summary', null, `${n.receivedAt ? fmtTime(n.receivedAt) : ''} ${(n.body && (n.body.statusUpdate || n.body.task || n.body.artifactUpdate)) ? Object.keys(n.body)[0] : 'notification'}`), jsonView(n, 'short'))))
        : h('p', { class: 'small muted' }, r.error ? r.error : 'No notifications yet. Send a message with push enabled.'));
    }
    const sinkBody = h('div');
    function renderSink() {
      clearInterval(sinkTimer);
      if (!st.push) { replace(sinkEl); return; }
      replace(sinkEl, card('Push notifications', { hint: '/a2a/webhook-sink/' + st.sinkId, actions: h('button', { class: 'btn sm', type: 'button', onclick: async () => { await send({ method: 'DELETE', url: target.base() + '/a2a/webhook-sink/' + st.sinkId }); pollSink(); } }, icon('trash'), 'Clear') },
        h('div', { class: 'stack-sm' }, h('p', { class: 'tiny muted' }, 'The agent POSTs task updates to this in-process sink (no outbound traffic). Polled every 2 s.'), sinkBody)));
      pollSink();
      sinkTimer = setInterval(pollSink, 2000);
    }

    loadDirectory();
    renderTask();
    renderSink();
    return () => { alive = false; clearInterval(sinkTimer); if (ctrl) ctrl.abort(); if (target.destroy) target.destroy(); };
  },
};

