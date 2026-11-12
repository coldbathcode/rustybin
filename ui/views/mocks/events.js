// The Channels tab of an AsyncAPI mock (#/mocks/{name}/channels): what each
// channel sends and receives, its pattern and faults (saved to the draft),
// a live feed of its messages, a composer that publishes a valid message as
// a client, a button that makes Rustybin publish now, and the webhook
// subscribers. The same on every surface: rustybin mocks channels | events |
// publish | subscribers, list_channels, list_channel_messages, publish_event.
import { h, replace, clear, card, field, input, select, textarea, notice, icon, toast, badge, fmtTime, copyButton, spinner } from '../../lib/dom.js';
import { api, report, errorText } from '../../lib/mockapi.js';
import { send, SERVER } from '../../lib/http.js';

const enc = encodeURIComponent;
const PATTERNS = [['', 'Default'], ['every', 'Every interval'], ['burst', 'A burst every interval'], ['on_receive', 'A reply to each message received'], ['off', 'Only when published by hand']];
const FEED_MS = 1000;
const FEED_KEPT = 50;

export function channelsPanel(ctx) {
  const listEl = h('div', { class: 'stack' });
  const subsEl = h('div', { class: 'stack-sm' });
  const channelSel = select([], '', { id: 'channel-select', 'aria-label': 'Channel' });
  const detailEl = h('div', { class: 'stack' });
  let channels = [];
  let current = '';
  let last = 0;
  let rows = [];
  let timer = null;
  let shown = false;
  let alive = true;

  const feedBody = h('tbody', { id: 'channel-feed' });
  const feedNote = h('p', { class: 'muted small', style: { margin: 0 } }, 'No message yet.');

  function messageRow(m) {
    const body = m.payload !== undefined ? JSON.stringify(m.payload) : (m.text || '');
    return h('tr', { class: m.valid ? '' : 'err-text' },
      h('td', { class: 'small mono nowrap' }, fmtTime(m.timestamp)),
      h('td', null, badge(m.direction, m.direction === 'sent' ? 'info' : '')),
      h('td', { class: 'small' }, m.message || '', m.fault ? [' ', badge(m.fault, 'warn')] : null, m.valid ? null : [' ', badge('not valid', 'err')]),
      h('td', { class: 'small mono', style: { wordBreak: 'break-all' } }, body + (m.truncated ? ' ...' : ''),
        m.issues && m.issues.length ? h('ul', { class: 'issue-list small' }, ...m.issues.map((i) => h('li', null, (i.pointer || '/') + ': ' + i.message))) : null),
      h('td', { class: 'small muted nowrap' }, m.delivered ? `${m.delivered.ws} ws, ${m.delivered.sse} sse, ${m.delivered.webhook} webhook` : (m.transport || '')));
  }

  function renderFeed() {
    clear(feedBody);
    for (const m of rows) feedBody.appendChild(messageRow(m));
    feedNote.hidden = rows.length > 0;
  }

  async function poll(reset) {
    if (!current) return;
    const channel = current;
    const r = await api('GET', `/${enc(ctx.name)}/channels/${enc(channel)}/messages?limit=${FEED_KEPT}${reset ? '' : '&after=' + last}`);
    if (!alive || channel !== current || !r.ok || !r.json) return;
    last = Math.max(reset ? 0 : last, r.json.last || 0);
    rows = (reset ? r.json.messages : r.json.messages.concat(rows)).slice(0, FEED_KEPT);
    renderFeed();
  }

  function startFeed() {
    clearInterval(timer);
    timer = setInterval(() => { if (shown && document.visibilityState !== 'hidden') poll(false); }, FEED_MS);
  }

  function findingsOf(id) {
    const d = ctx.detail();
    const rep = d && d.report && d.report.channels ? d.report.channels.find((c) => c.id === id) : null;
    return rep ? rep.findings : [];
  }

  function renderChannel() {
    const ch = channels.find((c) => c.id === current);
    if (!ch) { replace(detailEl, notice('info', 'The document has no channels.')); return; }
    const cfg = ch.settings;
    const patternSel = select(PATTERNS, cfg.pattern || '', { id: 'channel-pattern', 'aria-label': 'Pattern' });
    const everyIn = input({ id: 'channel-every', type: 'number', min: '100', max: '3600000', value: String(cfg.every_ms), 'aria-label': 'Interval in milliseconds' });
    const burstIn = input({ id: 'channel-burst', type: 'number', min: '1', max: '20', value: String(cfg.burst), 'aria-label': 'Messages in a burst' });
    const dropIn = input({ id: 'channel-drop', type: 'number', min: '0', max: '100', value: String(cfg.drop_percent), 'aria-label': 'Dropped, percent' });
    const cutIn = input({ id: 'channel-malformed', type: 'number', min: '0', max: '100', value: String(cfg.malformed_percent), 'aria-label': 'Malformed, percent' });
    const saveBtn = ctx.readOnly ? null : h('button', { class: 'btn sm', type: 'button', id: 'channel-save' }, 'Save to the draft');
    if (saveBtn) {
      saveBtn.onclick = async () => {
        const d = ctx.detail();
        const all = Object.assign({}, (d && d.settings && d.settings.channels) || {});
        const next = Object.assign({}, all[ch.id] || {}, {
          every_ms: Number(everyIn.value) || 1000,
          burst: Number(burstIn.value) || 5,
          drop_percent: Number(dropIn.value) || 0,
          malformed_percent: Number(cutIn.value) || 0,
        });
        if (patternSel.value) next.pattern = patternSel.value; else delete next.pattern;
        all[ch.id] = next;
        const r = await api('PATCH', '/' + enc(ctx.name), { channels: all }, { revision: d ? d.draft_revision : undefined });
        if (report(r, 'Saved to the draft: deploy it to change what the channel publishes.', 'Saving')) { await ctx.refresh(); load(); }
      };
    }
    const sample = ch.sample ? JSON.stringify(ch.sample.payload, null, 2) : '{}';
    const composer = textarea({ id: 'channel-compose', rows: 6, 'aria-label': 'Message to publish as a client' });
    composer.value = sample;
    const composeMsg = h('div');
    const sendBtn = h('button', { class: 'btn', type: 'button', id: 'channel-send' }, icon('send'), 'Publish as a client');
    sendBtn.onclick = async () => {
      sendBtn.disabled = true;
      const r = await send({ method: 'POST', url: SERVER + ch.path, headers: [['Content-Type', 'application/json']], body: composer.value, timeout: 20000 });
      sendBtn.disabled = false;
      if (r.status === 202) replace(composeMsg, notice('ok', 'Accepted as ' + (r.json && r.json.message ? r.json.message : 'a message') + (r.json && r.json.reply ? ': a reply was published.' : '.')));
      else if (r.status === 422 && r.json && r.json.error) replace(composeMsg, notice('err', 'Not valid: ', h('ul', { class: 'issue-list small' }, ...(r.json.error.issues || []).map((i) => h('li', null, (i.pointer || '/') + ': ' + i.message)))));
      else replace(composeMsg, notice('err', r.error || (r.json && r.json.error) || `${r.status} ${r.statusText}`));
      poll(false);
    };
    const publishBtn = h('button', { class: 'btn primary', type: 'button', id: 'channel-publish', disabled: ctx.readOnly || null }, icon('send'), 'Publish the next message');
    publishBtn.onclick = async () => {
      publishBtn.disabled = true;
      const r = await api('POST', `/${enc(ctx.name)}/channels/${enc(ch.id)}/publish`, {});
      publishBtn.disabled = false;
      if (!r.ok) toast(errorText(r, 'Publishing'), 'err');
      poll(false);
    };
    const names = (list) => list.length ? list.map((m) => m.name).join(', ') : 'nothing';
    const findings = findingsOf(ch.id);
    const wsUrl = ch.url.replace(/^http/, 'ws');
    replace(detailEl,
      card(ch.address, { hint: 'pattern: ' + ch.pattern.replace('_', ' ') },
        h('div', { class: 'stack-sm' },
          ch.description ? h('p', { class: 'small', style: { margin: 0 } }, ch.description) : null,
          h('div', { class: 'row' }, h('code', { class: 'mono small grow ellipsis', id: 'channel-url' }, wsUrl), copyButton(() => wsUrl, { label: 'Copy URL' })),
          h('p', { class: 'small muted', style: { margin: 0 } }, 'A WebSocket at this URL; the same URL over HTTP with Accept: text/event-stream for server-sent events; POST a message to it as a client.'),
          h('p', { class: 'small', style: { margin: 0 } }, h('strong', null, 'Publishes: '), names(ch.send), '. ', h('strong', null, 'Accepts: '), names(ch.receive), '.'),
          h('p', { class: 'small muted', style: { margin: 0 }, id: 'channel-live' }, `${ch.live.published} published, ${ch.live.ws} WebSocket and ${ch.live.sse} SSE connection(s), ${ch.live.webhooks} webhook subscriber(s).`),
          findings.length ? h('ul', { class: 'issue-list small' }, ...findings.map((f) => h('li', null, badge(f.level, f.level === 'error' ? 'err' : 'warn'), ' ', f.message))) : null)),
      card('Publishing', { hint: 'saved to the draft' },
        h('div', { class: 'row' }, field('Pattern', patternSel), field('Every (ms)', everyIn), field('Burst', burstIn), field('Dropped (%)', dropIn), field('Malformed (%)', cutIn)),
        h('div', { class: 'row' }, saveBtn, h('span', { class: 'grow' }), publishBtn)),
      ch.receive.length ? card('Publish as a client', { hint: 'validated against what the channel accepts' },
        h('div', { class: 'stack-sm' }, composer, h('div', { class: 'row' }, sendBtn), composeMsg)) : null,
      card('Messages', { hint: 'newest first' },
        feedNote,
        h('div', { class: 'table-wrap' }, h('table', { class: 'table' },
          h('thead', null, h('tr', null, h('th', null, 'Time'), h('th', null, ''), h('th', null, 'Message'), h('th', null, 'Payload'), h('th', null, 'To / from'))),
          feedBody))));
    rows = [];
    last = 0;
    renderFeed();
    poll(true);
  }

  // ── Webhook subscribers ──
  async function loadSubs() {
    const r = await api('GET', `/${enc(ctx.name)}/subscribers`);
    if (!alive) return;
    if (!r.ok || !r.json) { replace(subsEl, notice('err', errorText(r, 'The subscribers'))); return; }
    clear(subsEl);
    if (!r.json.subscribers.length) subsEl.appendChild(h('p', { class: 'muted small', style: { margin: 0 } }, 'No webhook subscriber.'));
    for (const s of r.json.subscribers) {
      const remove = ctx.readOnly ? null : h('button', { class: 'btn sm', type: 'button' }, icon('trash'), 'Remove');
      if (remove) remove.onclick = async () => { const d = await api('DELETE', `/${enc(ctx.name)}/subscribers/${enc(s.id)}`); if (report(d, 'Removed.', 'Removing')) loadSubs(); };
      subsEl.appendChild(h('div', { class: 'row' }, badge(s.channel, 'info'), h('span', { class: 'mono small grow ellipsis' }, s.url + (s.query_hidden ? '?...' : '')),
        h('span', { class: 'small muted' }, `${s.delivered} delivered, ${s.failed} failed`), remove));
    }
  }

  const hookUrl = input({ id: 'subscriber-url', placeholder: 'https://hooks.example.com/orders', 'aria-label': 'Webhook URL' });
  const hookBtn = h('button', { class: 'btn sm', type: 'button', id: 'subscriber-add' }, 'Add subscriber');
  hookBtn.onclick = async () => {
    if (!current || !hookUrl.value.trim()) return;
    const r = await api('POST', `/${enc(ctx.name)}/subscribers`, { channel: current, url: hookUrl.value.trim() });
    if (report(r, 'Subscriber added.', 'Adding the subscriber')) { hookUrl.value = ''; loadSubs(); load(); }
  };

  async function load() {
    const r = await api('GET', `/${enc(ctx.name)}/channels`);
    if (!alive) return;
    if (!r.ok || !r.json) { replace(listEl, notice('err', errorText(r, 'The channels'))); return; }
    channels = r.json.channels;
    if (!channels.some((c) => c.id === current)) current = channels.length ? channels[0].id : '';
    clear(channelSel);
    for (const c of channels) {
      const o = h('option', { value: c.id }, `${c.address} (${c.id})`);
      if (c.id === current) o.selected = true;
      channelSel.appendChild(o);
    }
    replace(listEl, h('div', { class: 'row' }, field('Channel', channelSel), h('span', { class: 'small muted' }, `AsyncAPI ${r.json.asyncapi}, ${channels.length} channel(s)`)));
    renderChannel();
  }
  channelSel.onchange = () => { current = channelSel.value; renderChannel(); };

  const el = h('div', { class: 'stack', id: 'channels-panel' },
    listEl,
    detailEl,
    ctx.visitorOwned ? null : card('Webhook subscribers', { hint: 'the selected channel\'s messages posted to a URL' },
      h('p', { class: 'small muted', style: { margin: 0 } }, 'The URL\'s host must be allowed for the outbound purpose events (RUSTYBIN_EGRESS_ALLOW; nothing is allowed by default). A subscriber lives an hour and is dropped after ten failed deliveries in a row.'),
      ctx.readOnly ? null : h('div', { class: 'row nowrap' }, h('div', { class: 'grow' }, hookUrl), hookBtn),
      subsEl));
  replace(listEl, spinner('Loading the channels'));

  return {
    el,
    show() {
      shown = true;
      load();
      if (!ctx.visitorOwned) loadSubs();
      startFeed();
    },
    hide() { shown = false; },
    destroy() { alive = false; clearInterval(timer); },
  };
}
