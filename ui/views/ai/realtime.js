// AI playground, realtime and the request log: a text realtime session
// over a WebSocket (GET /ai/openai/v1/realtime, the OpenAI-shaped realtime
// protocol) with its events as a timeline, and the mock's request log
// (GET /ai/requests) where a realtime session shows its events and a batch
// its lines. Audio is never part of either.
import { h, replace, card, notice, icon, badge, spinner, fmtTime, input } from '../../lib/dom.js';
import { jsonView } from '../../lib/format.js';
import { SERVER, send } from '../../lib/http.js';

const PATH = '/ai/openai/v1/realtime';

function wsUrl(model) {
  const base = SERVER.replace(/^http/, 'ws');
  return `${base}${PATH}?model=${encodeURIComponent(model)}`;
}

/** The events of a realtime session or record as a table. */
function timeline(events, id) {
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table', id },
    h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'Event'), h('th', null, 'Bytes'), h('th', null, 'Text'))),
    h('tbody', null, events.map((e) => h('tr', { 'data-event': e.type },
      h('td', null, badge(e.dir === 'in' ? 'client' : 'server', e.dir === 'in' ? 'info' : 'outline')),
      h('td', { class: 'mono small' }, e.type),
      h('td', { class: 'small' }, e.bytes != null ? String(e.bytes) : ''),
      h('td', { class: 'small' }, e.text || ''))))));
}

/** Try a text realtime session: connect, say something, read the events. */
export function realtimeCard() {
  let socket = null;
  let events = [];
  let reply = '';
  const model = input({ id: 'realtime-model', value: 'rustybin-realtime' });
  const prompt = input({ id: 'realtime-prompt', placeholder: 'Say something', value: 'Hello there' });
  const connectBtn = h('button', { class: 'btn', type: 'button', id: 'realtime-connect' }, icon('link'), 'Connect');
  const sendBtn = h('button', { class: 'btn primary', type: 'button', id: 'realtime-send', disabled: true }, icon('send'), 'Send');
  const closeBtn = h('button', { class: 'btn', type: 'button', id: 'realtime-close', disabled: true }, icon('stop'), 'Close');
  const status = h('span', { class: 'small muted', id: 'realtime-status' }, 'Not connected.');
  const replyEl = h('div', { class: 'chat', id: 'realtime-reply', 'aria-live': 'polite' });
  const eventsEl = h('div', { id: 'realtime-events' });

  function draw() {
    replace(replyEl, reply ? h('p', null, reply) : h('p', { class: 'muted small' }, 'The reply appears here as its text deltas arrive.'));
    replace(eventsEl, events.length ? timeline(events.slice(-200), 'realtime-timeline') : null);
  }

  function setOpen(open) {
    connectBtn.disabled = open;
    sendBtn.disabled = !open;
    closeBtn.disabled = !open;
  }

  connectBtn.onclick = () => {
    events = [];
    reply = '';
    draw();
    status.textContent = 'Connecting.';
    try {
      socket = new WebSocket(wsUrl(model.value.trim() || 'rustybin-realtime'));
    } catch (e) {
      status.textContent = 'Could not connect: ' + e.message;
      return;
    }
    socket.onopen = () => { status.textContent = 'Connected.'; setOpen(true); };
    socket.onclose = () => { status.textContent = 'Closed.'; setOpen(false); socket = null; };
    socket.onerror = () => { status.textContent = 'The connection failed (a gateway in between must pass WebSocket upgrades).'; };
    socket.onmessage = (m) => {
      let e;
      try { e = JSON.parse(m.data); } catch { return; }
      const text = e.type === 'response.text.delta' ? e.delta : (e.error && e.error.message) || '';
      if (e.type === 'response.text.delta') reply += e.delta;
      events.push({ dir: 'out', type: e.type, bytes: m.data.length, text });
      if (e.type === 'response.done') {
        const u = e.response && e.response.usage;
        if (u) status.textContent = `Connected. Last response: ${u.input_tokens} tokens in, ${u.output_tokens} out.`;
      }
      draw();
    };
  };

  function say(event) {
    const wire = JSON.stringify(event);
    events.push({ dir: 'in', type: event.type, bytes: wire.length, text: event.item ? event.item.content[0].text : '' });
    socket.send(wire);
  }

  sendBtn.onclick = () => {
    if (!socket || socket.readyState !== WebSocket.OPEN || !prompt.value.trim()) return;
    reply = '';
    say({ type: 'conversation.item.create', item: { type: 'message', role: 'user', content: [{ type: 'input_text', text: prompt.value }] } });
    say({ type: 'response.create' });
    draw();
  };
  prompt.addEventListener('keydown', (e) => { if (e.key === 'Enter') sendBtn.click(); });
  closeBtn.onclick = () => { if (socket) socket.close(); };

  draw();
  const el = card('Realtime session', { hint: 'WebSocket ' + PATH },
    h('p', { class: 'muted small' }, 'The OpenAI-shaped realtime protocol in text: events both ways over one WebSocket, answered by the same engine as the chat above. This panel connects to the instance itself.'),
    h('div', { class: 'row' },
      h('label', { class: 'field' }, h('span', null, 'Model'), model),
      h('label', { class: 'field grow' }, h('span', null, 'Say'), prompt),
      connectBtn, sendBtn, closeBtn),
    status, replyEl, eventsEl);
  return { el, destroy() { if (socket) socket.close(); } };
}

/** The mock's request log: every kind, with a session's events and a batch's lines. */
export function requestsCard() {
  const listEl = h('div', { id: 'ai-requests' }, spinner('Loading the request log'));
  const detailEl = h('div', { id: 'ai-request-detail' });
  const refresh = h('button', { class: 'btn', type: 'button', id: 'ai-requests-refresh', title: 'Refresh' }, icon('refresh'));

  async function show(id) {
    replace(detailEl, spinner('GET /ai/requests/' + id));
    const r = await send({ url: SERVER + '/ai/requests/' + encodeURIComponent(id), timeout: 10000 });
    if (!r.ok || !r.json) {
      replace(detailEl, notice('warn', r.error || (r.json && r.json.error) || `answered ${r.status}`));
      return;
    }
    const rec = r.json;
    const d = rec.detail || {};
    const head = h('p', { class: 'small' }, badge(rec.kind, 'info'), ' ', h('span', { class: 'mono' }, rec.request_id), ` ${rec.usage.prompt_tokens} tokens in, ${rec.usage.completion_tokens} out`);
    if (rec.kind === 'realtime') {
      replace(detailEl, h('div', { class: 'stack-sm' }, head,
        h('p', { class: 'small muted' }, `Session ${d.session_id}: ${d.responses} response(s). Event types, sizes and text only: audio is never kept.`),
        timeline(d.events || [], 'ai-request-timeline')));
    } else if (rec.kind === 'batch') {
      replace(detailEl, h('div', { class: 'stack-sm' }, head,
        h('p', { class: 'small muted' }, `Batch ${d.batch_id} of ${d.endpoint}: ${d.total} line(s), ${d.failing} failing.`),
        (d.validation_errors || []).length ? notice('warn', 'Validation failed: ' + d.validation_errors.map((e) => `line ${e.line}: ${e.message}`).join('; ')) : null,
        h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'ai-request-lines' },
          h('thead', null, h('tr', null, h('th', null, ''), h('th', null, 'custom_id'), h('th', null, 'Finish'), h('th', null, 'Tokens'), h('th', null, 'Text'))),
          h('tbody', null, (d.lines || []).map((l) => h('tr', null,
            h('td', null, badge(l.outcome, l.outcome === 'succeeded' ? 'ok' : 'err')),
            h('td', { class: 'mono small' }, l.custom_id),
            h('td', { class: 'small' }, l.finish || (l.status != null ? String(l.status) : '')),
            h('td', { class: 'small' }, l.tokens != null ? String(l.tokens) : ''),
            h('td', { class: 'small' }, l.text || ''))))))));
    } else {
      replace(detailEl, h('div', { class: 'stack-sm' }, head, jsonView(rec, 'tall')));
    }
  }

  async function load() {
    const r = await send({ url: SERVER + '/ai/requests?limit=20', timeout: 10000 });
    if (!r.ok || !r.json) {
      replace(listEl, notice('warn', r.error || `The request log answered ${r.status}.`));
      return;
    }
    if (!r.json.requests.length) {
      replace(listEl, h('p', { class: 'muted' }, 'No AI requests yet: send a chat above, open a realtime session or start a batch.'));
      return;
    }
    replace(listEl, h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'ai-requests-table' },
      h('thead', null, h('tr', null, h('th', null, 'Kind'), h('th', null, 'When'), h('th', null, 'Provider'), h('th', null, 'Endpoint'), h('th', null, 'Model'), h('th', null, 'Tokens'), h('th', null, ''))),
      h('tbody', null, r.json.requests.map((q) => h('tr', { 'data-ai-request': q.request_id, 'data-kind': q.kind },
        h('td', null, badge(q.kind, q.kind === 'request' ? 'outline' : 'info')),
        h('td', { class: 'small' }, fmtTime(q.timestamp)),
        h('td', { class: 'small' }, q.provider),
        h('td', { class: 'mono small' }, q.endpoint),
        h('td', { class: 'small' }, q.model),
        h('td', { class: 'small' }, `${q.prompt_tokens} / ${q.completion_tokens}`),
        h('td', null, h('button', { class: 'btn sm', type: 'button', title: 'Show this record', 'data-show-ai-request': q.request_id, onclick: () => show(q.request_id) }, icon('info')))))))));
  }

  refresh.onclick = load;
  load();
  return {
    el: card('Request log', { hint: 'GET /ai/requests', actions: refresh },
      h('p', { class: 'muted small' }, 'What the mock received, newest first: single requests, realtime sessions (their events) and batch jobs (their lines).'),
      listEl, detailEl),
    reload: load,
  };
}
