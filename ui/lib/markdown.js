// Safe Markdown: text becomes DOM nodes, never HTML. Headings, paragraphs,
// lists, fenced code with a copy button, `code`, **bold**, *italic*, and
// http(s) links (bare or [text](url)); any other URL scheme stays text.
// Used by the console's assist answers and the dev portal.
import { h, icon, copyText } from './dom.js';

function inline(text) {
  // `code`, **bold**, *italic*, [text](http(s) URL) and bare http(s) links.
  const out = [];
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|\[([^\]]+)\]\((https?:\/\/[^\s)<>"']+)\)|(https?:\/\/[^\s)<>"']+)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1]) out.push(h('code', null, m[1].slice(1, -1)));
    else if (m[2]) out.push(h('strong', null, m[2].slice(2, -2)));
    else if (m[3]) out.push(h('em', null, m[3].slice(1, -1)));
    else if (m[4]) out.push(h('a', { href: m[5], target: '_blank', rel: 'noopener noreferrer' }, m[4]));
    else if (m[6]) out.push(h('a', { href: m[6], target: '_blank', rel: 'noopener noreferrer' }, m[6]));
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/** Render markdown text to an element: headings, paragraphs, lists, code. Action blocks are left out. */
export function renderMarkdown(text) {
  const root = h('div', { class: 'md' });
  const lines = String(text || '').replace(/\r\n?/g, '\n').split('\n');
  let i = 0;
  let para = [];
  let list = null;
  const flushPara = () => { if (para.length) { root.appendChild(h('p', null, ...inline(para.join(' ')))); para = []; } };
  const flushList = () => { if (list) { root.appendChild(list.el); list = null; } };
  while (i < lines.length) {
    const line = lines[i];
    const fence = /^```\s*(\w+)?\s*$/.exec(line);
    if (fence) {
      flushPara(); flushList();
      const lang = (fence[1] || '').toLowerCase();
      const code = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { code.push(lines[i]); i++; }
      i++;
      if (lang === 'action') continue; // rendered as apply cards
      const body = code.join('\n');
      const pre = h('pre', { class: 'md-code' }, h('code', null, body));
      const bar = h('div', { class: 'md-code-bar' }, lang ? h('span', { class: 'muted small' }, lang) : h('span'), h('button', { class: 'btn sm ghost', type: 'button', onclick: () => copyText(body, 'Code') }, icon('copy'), 'Copy'));
      root.appendChild(h('div', { class: 'md-code-wrap' }, bar, pre));
      continue;
    }
    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (heading) {
      flushPara(); flushList();
      const level = Math.min(4, heading[1].length + 2);
      root.appendChild(h('h' + level, null, ...inline(heading[2])));
      i++;
      continue;
    }
    const item = /^\s*(?:[-*]|\d+[.)])\s+(.*)$/.exec(line);
    if (item) {
      flushPara();
      const ordered = /^\s*\d/.test(line);
      if (!list || list.ordered !== ordered) { flushList(); list = { ordered, el: h(ordered ? 'ol' : 'ul') }; }
      list.el.appendChild(h('li', null, ...inline(item[1])));
      i++;
      continue;
    }
    if (!line.trim()) { flushPara(); flushList(); i++; continue; }
    para.push(line.trim());
    i++;
  }
  flushPara(); flushList();
  return root;
}
