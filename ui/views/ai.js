// AI playground: chat with the mock LLM in each provider's native wire
// format (OpenAI chat, OpenAI Responses, Anthropic, Gemini, Ollama,
// Bedrock), with streaming, modes, fault and latency injection, tool calling
// and a raw wire panel showing the exact request and response frames.
import { h, replace, clear, card, field, input, textarea, select, notice, icon, badge, statusBadge, fmtMs, checkbox, tabs, copyButton, spinner, segmented } from '../lib/dom.js';
import { jsonView, codeBlock, headersTable, copyableCode } from '../lib/format.js';
import { SERVER, send, toCurl, sseParser, ndjsonParser, readText } from '../lib/http.js';
import { targetPicker } from '../lib/widgets.js';
import * as settings from '../lib/settings.js';

const WEATHER_TOOL = {
  name: 'get_weather',
  description: 'Get the current weather for a location',
  schema: { type: 'object', properties: { location: { type: 'string', description: 'City name' } }, required: ['location'] },
};

const PROVIDERS = {
  'openai-chat': { label: 'OpenAI chat', model: 'gpt-4o', stream: true, keyHint: 'Authorization: Bearer' },
  'openai-responses': { label: 'OpenAI Responses', model: 'gpt-4o', stream: true, keyHint: 'Authorization: Bearer' },
  anthropic: { label: 'Anthropic', model: 'claude-sonnet-4-5', stream: true, keyHint: 'x-api-key (plus anthropic-version)' },
  gemini: { label: 'Gemini', model: 'gemini-2.0-flash', stream: true, keyHint: 'x-goog-api-key' },
  ollama: { label: 'Ollama', model: 'llama3.2', stream: true, keyHint: 'Authorization: Bearer' },
  bedrock: { label: 'Bedrock', model: 'anthropic.claude-3-haiku-20240307-v1:0', stream: false, keyHint: 'Authorization: Bearer (Bedrock API key)' },
};

const FAULTS = [
  ['', 'No fault'], ['rate_limit', '429 rate limit'], ['server_error', '500 server error'], ['unavailable', '503 unavailable'],
  ['overloaded', '529 overloaded'], ['timeout', '504 timeout'], ['context_length', 'Context length exceeded'],
  ['content_filter', 'Content filter'], ['auth', '401 invalid credential'], ['rate_limit:50', '429 for 50% of requests'],
];

const SUGGESTIONS = ['Hello! Who are you?', 'What is the weather in Paris?', 'Tell me a joke', 'lorem 60', 'Summarize what an API gateway does'];

// ── Wire formats ───────────────────────────────────────────────────

function buildRequest(o) {
  const { provider, model, stream, tools, system, messages, base } = o;
  const headers = [['Content-Type', 'application/json']];
  let url, body;
  if (provider === 'openai-chat') {
    url = base + '/ai/openai/v1/chat/completions';
    const msgs = [];
    if (system) msgs.push({ role: 'system', content: system });
    for (const m of messages) {
      if (m.role === 'user') msgs.push({ role: 'user', content: m.text });
      else if (m.role === 'assistant') {
        const am = { role: 'assistant', content: m.text || null };
        if (m.toolCalls && m.toolCalls.length) am.tool_calls = m.toolCalls.map((c) => ({ id: c.id, type: 'function', function: { name: c.name, arguments: JSON.stringify(c.args) } }));
        msgs.push(am);
      } else if (m.role === 'tool') msgs.push({ role: 'tool', tool_call_id: m.toolCallId, content: JSON.stringify(m.result) });
    }
    body = { model, messages: msgs, stream };
    if (stream) body.stream_options = { include_usage: true };
    if (tools) body.tools = [{ type: 'function', function: { name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, parameters: WEATHER_TOOL.schema } }];
  } else if (provider === 'openai-responses') {
    url = base + '/ai/openai/v1/responses';
    const input = [];
    for (const m of messages) {
      if (m.role === 'user') input.push({ role: 'user', content: m.text });
      else if (m.role === 'assistant') {
        if (m.text) input.push({ role: 'assistant', content: m.text });
        for (const c of m.toolCalls || []) input.push({ type: 'function_call', call_id: c.id, name: c.name, arguments: JSON.stringify(c.args) });
      } else if (m.role === 'tool') input.push({ type: 'function_call_output', call_id: m.toolCallId, output: JSON.stringify(m.result) });
    }
    body = { model, input, stream };
    if (system) body.instructions = system;
    if (tools) body.tools = [{ type: 'function', name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, parameters: WEATHER_TOOL.schema }];
  } else if (provider === 'anthropic') {
    url = base + '/ai/anthropic/v1/messages';
    headers.push(['anthropic-version', '2023-06-01']);
    const msgs = [];
    for (const m of messages) {
      if (m.role === 'user') msgs.push({ role: 'user', content: m.text });
      else if (m.role === 'assistant') {
        const content = [];
        if (m.text) content.push({ type: 'text', text: m.text });
        for (const c of m.toolCalls || []) content.push({ type: 'tool_use', id: c.id, name: c.name, input: c.args });
        msgs.push({ role: 'assistant', content });
      } else if (m.role === 'tool') msgs.push({ role: 'user', content: [{ type: 'tool_result', tool_use_id: m.toolCallId, content: JSON.stringify(m.result) }] });
    }
    body = { model, max_tokens: 1024, messages: msgs, stream };
    if (system) body.system = system;
    if (tools) body.tools = [{ name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, input_schema: WEATHER_TOOL.schema }];
  } else if (provider === 'gemini') {
    url = base + '/ai/gemini/v1beta/models/' + encodeURIComponent(model) + (stream ? ':streamGenerateContent?alt=sse' : ':generateContent');
    const contents = [];
    for (const m of messages) {
      if (m.role === 'user') contents.push({ role: 'user', parts: [{ text: m.text }] });
      else if (m.role === 'assistant') {
        const parts = [];
        if (m.text) parts.push({ text: m.text });
        for (const c of m.toolCalls || []) parts.push({ functionCall: { name: c.name, args: c.args } });
        contents.push({ role: 'model', parts });
      } else if (m.role === 'tool') contents.push({ role: 'user', parts: [{ functionResponse: { name: m.name, response: m.result } }] });
    }
    body = { contents };
    if (system) body.systemInstruction = { parts: [{ text: system }] };
    if (tools) body.tools = [{ functionDeclarations: [{ name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, parameters: WEATHER_TOOL.schema }] }];
  } else if (provider === 'ollama') {
    url = base + '/ai/ollama/api/chat';
    const msgs = [];
    if (system) msgs.push({ role: 'system', content: system });
    for (const m of messages) {
      if (m.role === 'user') msgs.push({ role: 'user', content: m.text });
      else if (m.role === 'assistant') {
        const am = { role: 'assistant', content: m.text || '' };
        if (m.toolCalls && m.toolCalls.length) am.tool_calls = m.toolCalls.map((c) => ({ function: { name: c.name, arguments: c.args } }));
        msgs.push(am);
      } else if (m.role === 'tool') msgs.push({ role: 'tool', content: JSON.stringify(m.result), tool_name: m.name });
    }
    body = { model, messages: msgs, stream };
    if (tools) body.tools = [{ type: 'function', function: { name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, parameters: WEATHER_TOOL.schema } }];
  } else {
    url = base + '/ai/bedrock/model/' + encodeURIComponent(model) + '/converse';
    const msgs = [];
    for (const m of messages) {
      if (m.role === 'user') msgs.push({ role: 'user', content: [{ text: m.text }] });
      else if (m.role === 'assistant') {
        const content = [];
        if (m.text) content.push({ text: m.text });
        for (const c of m.toolCalls || []) content.push({ toolUse: { toolUseId: c.id, name: c.name, input: c.args } });
        msgs.push({ role: 'assistant', content });
      } else if (m.role === 'tool') msgs.push({ role: 'user', content: [{ toolResult: { toolUseId: m.toolCallId, content: [{ json: m.result }] } }] });
    }
    body = { messages: msgs, inferenceConfig: { maxTokens: 512 } };
    if (system) body.system = [{ text: system }];
    if (tools) body.toolConfig = { tools: [{ toolSpec: { name: WEATHER_TOOL.name, description: WEATHER_TOOL.description, inputSchema: { json: WEATHER_TOOL.schema } } }] };
  }
  return { method: 'POST', url, headers, body: JSON.stringify(body, null, 2) };
}

function addKey(provider, headers, key) {
  if (!key) return;
  if (provider === 'anthropic') headers.push(['x-api-key', key]);
  else if (provider === 'gemini') headers.push(['x-goog-api-key', key]);
  else headers.push(['Authorization', 'Bearer ' + key]);
}

/** Accumulates streamed or complete responses into {text, toolCalls, usage}. */
function makeAccumulator(provider) {
  const acc = { text: '', toolCalls: [], usage: null, finish: null };
  const partial = new Map(); // index -> {id, name, args}
  const parseArgs = (s) => { try { return typeof s === 'string' ? (s ? JSON.parse(s) : {}) : (s || {}); } catch { return { _raw: s }; } };
  function finishPartials() {
    for (const p of partial.values()) acc.toolCalls.push({ id: p.id, name: p.name, args: parseArgs(p.args) });
    partial.clear();
  }
  const handlers = {
    'openai-chat': (j) => {
      const ch = j.choices && j.choices[0];
      if (ch && ch.delta) {
        if (ch.delta.content) acc.text += ch.delta.content;
        for (const tc of ch.delta.tool_calls || []) {
          const p = partial.get(tc.index) || { id: '', name: '', args: '' };
          if (tc.id) p.id = tc.id;
          if (tc.function && tc.function.name) p.name = tc.function.name;
          if (tc.function && tc.function.arguments) p.args += tc.function.arguments;
          partial.set(tc.index, p);
        }
        if (ch.finish_reason) acc.finish = ch.finish_reason;
      } else if (ch && ch.message) {
        acc.text += ch.message.content || '';
        for (const tc of ch.message.tool_calls || []) acc.toolCalls.push({ id: tc.id, name: tc.function.name, args: parseArgs(tc.function.arguments) });
        acc.finish = ch.finish_reason;
      }
      if (j.usage) acc.usage = { input: j.usage.prompt_tokens, output: j.usage.completion_tokens };
    },
    'openai-responses': (j) => {
      if (j.type === 'response.output_text.delta') acc.text += j.delta || '';
      else if (j.type === 'response.output_item.done' && j.item && j.item.type === 'function_call') acc.toolCalls.push({ id: j.item.call_id, name: j.item.name, args: parseArgs(j.item.arguments) });
      else if (j.type === 'response.completed' && j.response) {
        if (j.response.usage) acc.usage = { input: j.response.usage.input_tokens, output: j.response.usage.output_tokens };
        acc.finish = j.response.status;
      } else if (j.object === 'response') {
        for (const item of j.output || []) {
          if (item.type === 'message') for (const c of item.content || []) acc.text += c.text || '';
          if (item.type === 'function_call') acc.toolCalls.push({ id: item.call_id, name: item.name, args: parseArgs(item.arguments) });
        }
        if (j.usage) acc.usage = { input: j.usage.input_tokens, output: j.usage.output_tokens };
        acc.finish = j.status;
      }
    },
    anthropic: (j) => {
      if (j.type === 'message_start' && j.message && j.message.usage) acc.usage = { input: j.message.usage.input_tokens, output: j.message.usage.output_tokens };
      else if (j.type === 'content_block_start' && j.content_block && j.content_block.type === 'tool_use') partial.set(j.index, { id: j.content_block.id, name: j.content_block.name, args: '' });
      else if (j.type === 'content_block_delta' && j.delta) {
        if (j.delta.type === 'text_delta') acc.text += j.delta.text;
        if (j.delta.type === 'input_json_delta' && partial.has(j.index)) partial.get(j.index).args += j.delta.partial_json;
      } else if (j.type === 'message_delta') {
        if (j.usage) acc.usage = Object.assign(acc.usage || {}, { output: j.usage.output_tokens });
        if (j.delta && j.delta.stop_reason) acc.finish = j.delta.stop_reason;
      } else if (j.type === 'message') {
        for (const c of j.content || []) {
          if (c.type === 'text') acc.text += c.text;
          if (c.type === 'tool_use') acc.toolCalls.push({ id: c.id, name: c.name, args: c.input || {} });
        }
        if (j.usage) acc.usage = { input: j.usage.input_tokens, output: j.usage.output_tokens };
        acc.finish = j.stop_reason;
      }
    },
    gemini: (j) => {
      const cand = j.candidates && j.candidates[0];
      for (const p of (cand && cand.content && cand.content.parts) || []) {
        if (p.text) acc.text += p.text;
        if (p.functionCall) acc.toolCalls.push({ id: 'call_' + acc.toolCalls.length, name: p.functionCall.name, args: p.functionCall.args || {} });
      }
      if (cand && cand.finishReason) acc.finish = cand.finishReason;
      if (j.usageMetadata) acc.usage = { input: j.usageMetadata.promptTokenCount, output: j.usageMetadata.candidatesTokenCount };
    },
    ollama: (j) => {
      if (j.message) {
        acc.text += j.message.content || '';
        for (const tc of j.message.tool_calls || []) acc.toolCalls.push({ id: 'call_' + acc.toolCalls.length, name: tc.function.name, args: tc.function.arguments || {} });
      }
      if (j.done) {
        acc.usage = { input: j.prompt_eval_count, output: j.eval_count };
        acc.finish = j.done_reason;
      }
    },
    bedrock: (j) => {
      const content = (j.output && j.output.message && j.output.message.content) || [];
      for (const c of content) {
        if (c.text) acc.text += c.text;
        if (c.toolUse) acc.toolCalls.push({ id: c.toolUse.toolUseId, name: c.toolUse.name, args: c.toolUse.input || {} });
      }
      if (j.usage) acc.usage = { input: j.usage.inputTokens, output: j.usage.outputTokens };
      acc.finish = j.stopReason;
    },
  };
  return { acc, feed: handlers[provider], done: finishPartials };
}

function fakeWeather(args) {
  const loc = (args && (args.location || args.city)) || 'Paris';
  return { location: loc, temperature_c: 21, conditions: 'partly cloudy', humidity: 0.54, source: 'rustybin console demo tool' };
}

// ── View ───────────────────────────────────────────────────────────

export default {
  id: 'ai',
  mount(root) {
    let alive = true;
    let ctrl = null;
    const st = settings.recall('ai.state', {
      provider: 'openai-chat', model: '', stream: true, mode: '', fault: '', ttft: '', tps: '', key: '', requireAuth: false, tools: false, system: '',
      messages: [], exchanges: [],
    });
    settings.remember('ai.state', st);
    const target = targetPicker('ai');

    const chatEl = h('div', { class: 'chat', 'aria-live': 'polite' });
    const wireEl = h('div');
    const promptIn = textarea({ rows: 1, placeholder: 'Message the mock model (Enter sends, Shift+Enter for a new line)', class: 'textarea', 'aria-label': 'Message' });
    const sendBtn = h('button', { class: 'btn primary', type: 'button' }, icon('send'), 'Send');
    const stopBtn = h('button', { class: 'btn', type: 'button', hidden: true }, icon('stop'), 'Stop');

    // Settings controls
    const providerSeg = segmented(Object.entries(PROVIDERS).map(([k, v]) => [k, v.label]), st.provider, (v) => { st.provider = v; modelIn.value = ''; modelIn.placeholder = PROVIDERS[v].model; syncStream(); renderWire(); }, 'Provider');
    const modelIn = input({ value: st.model, placeholder: PROVIDERS[st.provider].model, class: 'input mono' });
    modelIn.addEventListener('input', () => { st.model = modelIn.value.trim(); });
    const modeSel = select([['', 'Auto (from the model name)'], ['canned', 'canned'], ['echo', 'echo'], ['scripted', 'scripted'], ['random', 'random']], st.mode);
    modeSel.addEventListener('change', () => { st.mode = modeSel.value; });
    const faultSel = select(FAULTS, st.fault);
    faultSel.addEventListener('change', () => { st.fault = faultSel.value; });
    const ttftIn = input({ type: 'number', min: '0', placeholder: '0', value: st.ttft, class: 'input mono' });
    ttftIn.addEventListener('input', () => { st.ttft = ttftIn.value; });
    const tpsIn = input({ type: 'number', min: '0', placeholder: '100', value: st.tps, class: 'input mono' });
    tpsIn.addEventListener('input', () => { st.tps = tpsIn.value; });
    const keyIn = input({ type: 'password', value: st.key, placeholder: 'optional, e.g. sk-demo-1234', class: 'input mono' });
    keyIn.addEventListener('input', () => { st.key = keyIn.value.trim(); });
    const keyHint = h('span');
    const streamCb = checkbox('Stream', st.stream, (v) => { st.stream = v; });
    const toolsCb = checkbox('Offer the get_weather tool', st.tools, (v) => { st.tools = v; });
    const authCb = checkbox('Require credentials', st.requireAuth, (v) => { st.requireAuth = v; });
    const systemIn = input({ value: st.system, placeholder: 'Optional system prompt' });
    systemIn.addEventListener('input', () => { st.system = systemIn.value; });

    function syncStream() {
      const p = PROVIDERS[st.provider];
      streamCb.input.disabled = !p.stream;
      if (!p.stream) streamCb.input.checked = false;
      else streamCb.input.checked = st.stream;
      keyHint.textContent = 'Sent as ' + p.keyHint;
    }
    syncStream();

    root.appendChild(h('div', { class: 'view-head' },
      h('div', { class: 'grow' }, h('h1', null, 'AI playground'),
        h('p', null, 'Chat with the mock LLM in each provider\'s native wire format. Send through the gateway to demo AI proxying, credential injection, token rate limits and guardrails; the raw wire panel shows exactly what went over the network.')),
      h('div', { class: 'row' }, target.el)));

    root.appendChild(h('div', { class: 'stack' },
      card(null, null, h('div', { class: 'stack-sm' },
        h('div', { class: 'row' }, h('span', { class: 'label' }, 'Provider'), providerSeg.el),
        h('div', { class: 'grid grid-4' },
          field('Model', modelIn),
          field('Mode (X-Rustybin-Mode)', modeSel),
          field('Fault (X-Rustybin-Fail)', faultSel),
          field('API key', keyIn, keyHint)),
        h('div', { class: 'grid grid-4' },
          field('Time to first token (ms)', ttftIn),
          field('Tokens per second', tpsIn, '0 = unpaced'),
          field('System prompt', systemIn),
          h('div', { class: 'stack-sm', style: { justifyContent: 'flex-end' } }, streamCb.el, toolsCb.el, authCb.el)))),
      h('div', { class: 'grid grid-2' },
        h('div', { class: 'card' },
          h('div', { class: 'card-head' }, h('h2', null, 'Conversation'),
            h('button', { class: 'btn sm', type: 'button', onclick: () => { st.messages = []; st.exchanges = []; renderChat(); renderWire(); } }, icon('trash'), 'New chat')),
          chatEl,
          h('div', { class: 'chat-input' }, promptIn, sendBtn, stopBtn)),
        wireEl)));

    promptIn.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); submit(); }
    });
    sendBtn.addEventListener('click', submit);
    stopBtn.addEventListener('click', () => { if (ctrl) ctrl.abort(); });

    function submit() {
      const text = promptIn.value.trim();
      if (!text || ctrl) return;
      promptIn.value = '';
      st.messages.push({ role: 'user', text });
      run();
    }

    // ── Chat rendering ──
    function renderChat() {
      clear(chatEl);
      if (!st.messages.length) {
        chatEl.appendChild(h('div', { class: 'empty' }, h('h3', null, 'Start a conversation'),
          h('p', null, 'Try one of these:'),
          h('div', { class: 'pill-list', style: { justifyContent: 'center' } }, ...SUGGESTIONS.map((s) => h('button', { class: 'btn sm', type: 'button', onclick: () => { promptIn.value = s; if (s.includes('weather') && !st.tools) { st.tools = true; toolsCb.input.checked = true; } submit(); } }, s)))));
        return;
      }
      st.messages.forEach((m, i) => chatEl.appendChild(bubble(m, i)));
      chatEl.scrollTop = chatEl.scrollHeight;
    }

    function bubble(m, i) {
      if (m.role === 'user') return h('div', { class: 'msg user' }, m.text);
      if (m.role === 'tool') return h('div', { class: 'msg tool' }, h('div', { class: 'tiny muted' }, `tool result for ${m.name}`), JSON.stringify(m.result, null, 2));
      if (m.role === 'error') return h('div', { class: 'msg error' }, h('strong', null, m.title), h('div', { class: 'small', style: { marginTop: '4px' } }, m.text), meta(m));
      const el = h('div', { class: 'msg assistant' + (m.streaming ? ' cursor' : '') });
      if (m.text) el.appendChild(document.createTextNode(m.text));
      if (m.streaming && !m.text) el.appendChild(h('span', { class: 'muted small' }, 'waiting for the first token...'));
      for (const c of m.toolCalls || []) {
        const last = i === st.messages.length - 1;
        el.appendChild(h('div', { class: 'artifact' },
          h('div', { class: 'row' }, badge('tool call', 'violet'), h('code', null, c.name)),
          codeBlock(JSON.stringify(c.args, null, 2), 'json', 'short'),
          last && !ctrl ? h('button', { class: 'btn sm primary', type: 'button', style: { marginTop: '8px' }, onclick: () => sendToolResult(c) }, 'Send tool result') : null));
      }
      el.appendChild(meta(m));
      return el;
    }

    function meta(m) {
      if (!m.meta) return h('span');
      const x = m.meta;
      return h('div', { class: 'meta' },
        x.status !== undefined ? statusBadge(x.status) : null,
        x.ttfb !== undefined ? h('span', null, 'first byte ' + fmtMs(x.ttfb)) : null,
        x.total !== undefined ? h('span', null, 'total ' + fmtMs(x.total)) : null,
        x.usage ? h('span', null, `${x.usage.input ?? '?'} in / ${x.usage.output ?? '?'} out tokens`) : null,
        x.finish ? h('span', null, 'finish: ' + x.finish) : null,
        x.requestId ? h('a', { href: SERVER + '/ai/requests/' + encodeURIComponent(x.requestId), target: '_blank', rel: 'noopener', title: 'What the upstream received' }, 'upstream record') : null);
    }

    function sendToolResult(call) {
      st.messages.push({ role: 'tool', toolCallId: call.id, name: call.name, result: fakeWeather(call.args) });
      run();
    }

    // ── Sending ──
    async function run() {
      const provider = st.provider;
      const model = st.model || PROVIDERS[provider].model;
      const stream = PROVIDERS[provider].stream && st.stream;
      const history = st.messages.filter((m) => m.role !== 'error');
      const req = buildRequest({ provider, model, stream, tools: st.tools, system: st.system.trim(), messages: history, base: target.base() });
      addKey(provider, req.headers, st.key);
      if (st.mode) req.headers.push(['X-Rustybin-Mode', st.mode]);
      if (st.fault) req.headers.push(['X-Rustybin-Fail', st.fault]);
      if (st.ttft) req.headers.push(['X-Rustybin-TTFT-Ms', String(Number(st.ttft) || 0)]);
      if (st.tps !== '' && st.tps !== undefined) req.headers.push(['X-Rustybin-Tokens-Per-Second', String(Number(st.tps) || 0)]);
      if (st.requireAuth) req.headers.push(['X-Rustybin-Require-Auth', 'true']);

      const assistant = { role: 'assistant', text: '', toolCalls: [], streaming: true, meta: null };
      st.messages.push(assistant);
      const exchange = { provider, request: req, status: null, headers: [], frames: [], body: null, started: performance.now(), requestId: null, error: null };
      st.exchanges.unshift(exchange);
      if (st.exchanges.length > 20) st.exchanges.length = 20;
      ctrl = new AbortController();
      sendBtn.hidden = true;
      stopBtn.hidden = false;
      renderChat();
      renderWire();

      const r = await send(Object.assign({ stream: true, timeout: 90000, signal: ctrl.signal }, req));
      if (!alive) return;
      exchange.status = r.status;
      exchange.headers = r.headers;
      exchange.requestId = r.header('x-rustybin-request-id');
      const { acc, feed, done } = makeAccumulator(provider);
      let renderPending = false;
      const repaint = () => {
        if (renderPending) return;
        renderPending = true;
        requestAnimationFrame(() => { renderPending = false; if (alive) { assistant.text = acc.text; renderChat(); renderWire(); } });
      };
      try {
        if (r.error) throw new Error(r.error);
        const ct = r.contentType;
        if (!r.ok) {
          const text = await r.response.text();
          exchange.body = text;
          let parsed = null;
          try { parsed = JSON.parse(text); } catch { /* not json */ }
          const msg = parsed ? (parsed.error && (parsed.error.message || parsed.error) || parsed.message || parsed.Message || text) : text;
          finishError(`${r.status} ${r.statusText || ''}`.trim() + (r.header('x-rustybin-fault') ? ' (injected fault)' : ''), typeof msg === 'string' ? msg : JSON.stringify(msg), r);
          return;
        }
        if (ct.includes('event-stream')) {
          const parse = sseParser((ev) => {
            exchange.frames.push({ t: performance.now() - exchange.started, text: (ev.event !== 'message' ? `event: ${ev.event}\n` : '') + 'data: ' + ev.data });
            if (ev.data === '[DONE]') return;
            try { feed(JSON.parse(ev.data)); } catch { /* ignore non-JSON frames */ }
            repaint();
          });
          await readText(r.response, parse, ctrl.signal);
        } else if (ct.includes('ndjson') || provider === 'ollama') {
          const parse = ndjsonParser((line) => {
            exchange.frames.push({ t: performance.now() - exchange.started, text: line });
            try { feed(JSON.parse(line)); } catch { /* ignore */ }
            repaint();
          });
          await readText(r.response, parse, ctrl.signal);
        } else {
          const text = await r.response.text();
          exchange.body = text;
          try {
            const j = JSON.parse(text);
            if (Array.isArray(j)) j.forEach(feed); else feed(j);
          } catch { acc.text = text; }
        }
        done();
        assistant.text = acc.text;
        assistant.toolCalls = acc.toolCalls;
        assistant.streaming = false;
        assistant.meta = { status: r.status, ttfb: r.ttfbMs, total: performance.now() - exchange.started, usage: acc.usage, finish: acc.finish, requestId: exchange.requestId };
        if (!assistant.text && !assistant.toolCalls.length) assistant.text = '(empty response)';
      } catch (e) {
        if (ctrl && ctrl.signal.aborted) {
          assistant.streaming = false;
          assistant.text = (acc.text || '') + ' [stopped]';
          assistant.meta = { status: r.status, total: performance.now() - exchange.started, requestId: exchange.requestId };
        } else {
          finishError('Request failed', e.message, r);
          return;
        }
      } finally {
        exchange.total = performance.now() - exchange.started;
        ctrl = null;
        sendBtn.hidden = false;
        stopBtn.hidden = true;
        if (alive) { renderChat(); renderWire(); }
      }

      function finishError(title, text, resp) {
        const idx = st.messages.indexOf(assistant);
        st.messages.splice(idx, 1, { role: 'error', title, text, meta: { status: resp.status, total: performance.now() - exchange.started, requestId: exchange.requestId } });
        exchange.error = text;
      }
    }

    // ── Raw wire panel ──
    let wireTab = settings.recall('ai.wireTab', 'request');
    function renderWire() {
      const ex = st.exchanges[0];
      if (!ex) {
        const preview = buildRequest({ provider: st.provider, model: st.model || PROVIDERS[st.provider].model, stream: PROVIDERS[st.provider].stream && st.stream, tools: st.tools, system: st.system.trim(), messages: [{ role: 'user', text: 'Hello! Who are you?' }], base: target.base() });
        replace(wireEl, card('Raw wire', { hint: 'what the next request looks like' }, h('div', { class: 'stack-sm' },
          h('div', { class: 'mono small break' }, 'POST ' + preview.url),
          headersTable(preview.headers, { noHighlight: true }),
          jsonView(preview.body, 'tall'))));
        return;
      }
      const curl = toCurl(Object.assign({ stream: true }, ex.request));
      const items = [
        { id: 'request', label: 'Request', render: () => h('div', { class: 'card-body stack-sm' },
          h('div', { class: 'mono small break' }, ex.request.method + ' ' + ex.request.url),
          headersTable(ex.request.headers, { noHighlight: true }), jsonView(ex.request.body, 'tall')) },
        { id: 'response', label: ex.frames.length ? `Response (${ex.frames.length} frames)` : 'Response', render: () => h('div', { class: 'card-body stack-sm' },
          ex.status !== null ? h('div', { class: 'row' }, statusBadge(ex.status), ex.total ? h('span', { class: 'muted small' }, fmtMs(ex.total)) : spinner('streaming')) : spinner('waiting for headers'),
          ex.headers.length ? h('details', { class: 'disclosure' }, h('summary', null, `Response headers (${ex.headers.length})`), headersTable(ex.headers, { noHighlight: true })) : null,
          ex.frames.length ? framesView(ex.frames) : ex.body !== null ? jsonView(ex.body, 'tall') : null) },
        { id: 'upstream', label: 'Upstream record', render: () => upstreamView(ex) },
        { id: 'curl', label: 'curl', render: () => h('div', { class: 'card-body' }, copyableCode(curl)) },
      ];
      replace(wireEl, h('div', { class: 'card' },
        h('div', { class: 'card-head' }, h('h2', null, 'Raw wire'), ex.requestId ? h('span', { class: 'hint mono' }, ex.requestId) : null, copyButton(curl, { label: 'Copy as curl' })),
        tabs(items, wireTab, (id) => { wireTab = id; settings.remember('ai.wireTab', id); }).el));
    }

    function framesView(frames) {
      const pre = h('pre', { class: 'code tall', tabindex: '0' });
      for (const f of frames.slice(-400)) {
        pre.appendChild(h('span', { class: 'j-null' }, `+${Math.round(f.t)}ms `));
        pre.appendChild(document.createTextNode(f.text + '\n'));
      }
      return pre;
    }

    function upstreamView(ex) {
      const host = h('div', { class: 'card-body' });
      if (!ex.requestId) {
        replace(host, notice('', 'Available once the response headers arrived (X-Rustybin-Request-Id).'));
        return host;
      }
      replace(host, spinner('GET /ai/requests/' + ex.requestId));
      send({ url: SERVER + '/ai/requests/' + encodeURIComponent(ex.requestId), timeout: 10000 }).then((r) => {
        if (!alive) return;
        if (r.ok && r.json) {
          replace(host, h('div', { class: 'stack-sm' },
            h('p', { class: 'small muted' }, 'What Rustybin received for this call (credentials redacted): useful to prove what a gateway injected or rewrote.'),
            r.json.credential ? h('div', null, badge('credential seen: ' + r.json.credential, 'info')) : null,
            jsonView(r.json, 'tall')));
        } else {
          replace(host, notice('warn', r.error || (r.json && r.json.error) || `answered ${r.status}`, r.status === 404 ? ' (through a gateway the record may belong to another session)' : ''));
        }
      });
      return host;
    }

    renderChat();
    renderWire();
    return () => { alive = false; if (ctrl) ctrl.abort(); if (target.destroy) target.destroy(); };
  },
};

