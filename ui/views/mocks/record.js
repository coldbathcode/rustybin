// Recordings of a real API (#/mocks, "Record from an API"): start one with a
// name and the API's base URL, send traffic to the proxy URL it answers,
// then build a mock from what it kept (a spec inferred by code, the recorded
// answers as examples). /_rustybin/recordings*; the same on every surface:
// rustybin record, the *_recording tools. What a recording kept is what the
// real API answered: shown as text, never as markup.
import { h, replace, clear, card, field, input, notice, icon, spinner, toast, fmtTime } from '../../lib/dom.js';
import { copyableCode } from '../../lib/format.js';
import { send, SERVER } from '../../lib/http.js';

const BASE = '/_rustybin/recordings';

async function call(method, path, body) {
  const opts = { control: true, method, url: SERVER + path };
  if (body !== undefined) {
    opts.headers = { 'Content-Type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  const r = await send(opts);
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
  return r.json;
}

export function recordPanel({ onBuilt } = {}) {
  const listEl = h('div', { class: 'stack', id: 'recordings' }, spinner('Loading recordings'));
  const name = input({ id: 'recording-name', placeholder: 'orders', maxlength: 40, 'aria-label': 'Recording name' });
  const upstream = input({ id: 'recording-upstream', placeholder: 'https://api.example.com/v1', 'aria-label': 'Upstream base URL' });
  const startBtn = h('button', { class: 'btn primary', type: 'button', id: 'recording-start' }, icon('play'), 'Start recording');
  startBtn.onclick = async () => {
    startBtn.disabled = true;
    try {
      const r = await call('POST', BASE, { name: name.value.trim(), upstream: upstream.value.trim() });
      toast(r.egress && r.egress.allowed === false ? 'Started: the outbound policy does not allow the upstream yet' : 'Recording: send requests to its proxy URL');
      name.value = '';
      await load();
    } catch (e) {
      toast('Could not start: ' + e.message, 'err');
    } finally {
      startBtn.disabled = false;
    }
  };
  const el = card('Record from an API', { hint: 'a proxy in front of the real API; then a mock built from what it saw' },
    h('p', { class: 'small muted', style: { margin: 0 } }, 'Requests sent to the proxy URL go to the real API (its host must be allowed for the outbound purpose record) and come back unchanged; the exchanges are kept without credentials. Build makes a mock: a spec inferred from the traffic, the recorded answers as examples. Review them before publishing: they are what the real API answered.'),
    h('div', { class: 'row' }, field('Name', name), field('Upstream base URL', upstream), startBtn),
    listEl);
  el.id = 'record-panel';

  function row(r) {
    const path = `${BASE}/${encodeURIComponent(r.name)}`;
    const out = h('div', { class: 'stack-sm' });
    const act = (label, ic, fn, id) => {
      const b = h('button', { class: 'btn sm', type: 'button', id: `${id}-${r.name}` }, icon(ic), label);
      b.onclick = async () => {
        b.disabled = true;
        try { await fn(); } catch (e) { toast(e.message, 'err'); } finally { b.disabled = false; }
      };
      return b;
    };
    const preview = act('Preview', 'diff', async () => {
      const v = await call('POST', path + '/build', { dry_run: true });
      replace(out, buildView(v));
    }, 'recording-preview');
    const build = act('Build mock', 'plus', async () => {
      const v = await call('POST', path + '/build', {});
      replace(out, buildView(v));
      toast(`Mock ${v.mock} built from ${v.operations.length} operation(s)`);
      if (onBuilt) onBuilt(v.mock);
    }, 'recording-build');
    const stop = act('Stop', 'stop', async () => { await call('POST', path + '/stop'); await load(); }, 'recording-stop');
    const del = act('Delete', 'trash', async () => {
      if (!window.confirm(`Delete recording ${r.name} and what it kept? The mock it built stays.`)) return;
      await call('DELETE', path);
      await load();
    }, 'recording-delete');
    return h('div', { class: 'card', 'data-recording': r.name, style: { padding: '10px 14px' } },
      h('div', { class: 'row' },
        h('div', { class: 'grow' }, h('strong', null, r.name), ' ', h('span', { class: 'muted small' }, `${r.state}, ${r.exchanges} exchange(s), to ${r.upstream}, expires ${fmtTime(r.expires_at)}`)),
        r.state === 'recording' ? stop : null, preview, build, del),
      r.state === 'recording' ? h('div', { class: 'small' }, 'Send requests to:', copyableCode(r.proxy_url)) : null,
      r.not_kept ? notice('warn', `${r.not_kept} exchange(s) forwarded but not kept (a recording keeps ${r.limits.exchanges} at most)`) : null,
      r.refused ? notice('warn', `${r.refused} request(s) refused (stopped, or the outbound policy)`) : null,
      out);
  }

  function buildView(v) {
    const ops = h('ul', { class: 'small' }, ...v.operations.map((o) => h('li', null, `${o.method} ${o.path} (${o.exchanges} exchange(s), ${o.statuses.join(', ')})`)));
    return h('div', { class: 'stack-sm' },
      notice(v.dry_run ? 'info' : 'ok', v.dry_run ? `Would build mock ${v.mock}: ${v.operations.length} operation(s), ${v.stored_examples} example(s)` : `Built mock ${v.mock}`),
      ops,
      v.personal_data.length ? notice('warn', 'What looks like personal data in the recorded answers: ' + v.personal_data.map((f) => `${f.operation} (${f.kinds.join(', ')})`).join('; ') + '. Review the examples before publishing.') : null);
  }

  async function load() {
    try {
      const d = await call('GET', BASE);
      clear(listEl);
      if (!d.recordings.length) listEl.appendChild(h('p', { class: 'muted small' }, 'No recordings in this session.'));
      for (const r of d.recordings) listEl.appendChild(row(r));
    } catch (e) {
      replace(listEl, notice('err', 'The recordings could not be loaded: ' + e.message));
    }
  }
  load();
  return el;
}
