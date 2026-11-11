// The README's animated pictures: standalone SVG ports of the home page
// illustrations (src/components/viz/). GitHub shows a README SVG through
// <img>, which runs CSS animations but loads no web fonts, so every text
// run gets a textLength: glyphs from any monospace font land on the same
// grid and nothing overlaps.
//
//   node site/scripts/readme-art.mjs   ->  docs/readme/{keys,tabs,ssh,agent}.svg
import { writeFileSync } from 'node:fs';

const OUT = new URL('../../docs/readme/', import.meta.url);
const W = 480;
const CH = 7.8; // one cell of 13px monospace
const C = {
  bg: '#1f2335', bar: '#1a1b26', edge: '#2f3449', fg: '#c0caf5', dim: '#737aa2', bartext: '#8089b3',
  blue: '#7aa2f7', ink: '#16161e', cap: '#2a2f45', capfg: '#a9b1d6', green: '#9ece6a', red: '#f7768e',
  purple: '#bb9af7', cyan: '#7dcfff',
};

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
/** A text run of `cells` columns (default: its length) at a fixed width. */
const t = (x, y, s, attrs = '', cells = s.length) => {
  // SVG drops leading and trailing spaces, so they become an x offset instead.
  const lead = s.length - s.trimStart().length;
  const body = s.trim();
  const len = cells === s.length ? body.length : cells;
  return `<text x="${(x + lead * CH).toFixed(1)}" y="${y}" textLength="${(len * CH).toFixed(1)}" ${attrs}>${esc(body)}</text>`;
};
/** Several coloured spans on one line, laid out cell by cell. */
const line = (x, y, parts, attrs = '') =>
  parts
    .map(([s, fill]) => {
      const out = s.trim() ? t(x, y, s, `fill="${fill}" ${attrs}`) : '';
      x += s.length * CH;
      return out;
    })
    .join('');

function svg(name, h, label, title, css, body) {
  const bar = title
    ? `<path d="M10 0.5h${W - 20}a9.5 9.5 0 0 1 9.5 9.5v20.5h-${W - 1}v-20.5a9.5 9.5 0 0 1 9.5-9.5z" fill="${C.bar}"/>` +
      t(16, 20, title, `fill="${C.bartext}" font-size="12"`, title.length * 0.92)
    : '';
  const doc = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${h}" viewBox="0 0 ${W} ${h}" role="img" aria-label="${esc(label)}">
<title>${esc(label)}</title>
<style>
text { font-family: 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', 'Liberation Mono', monospace; font-size: 13px; }
.sans { font-family: -apple-system, 'Segoe UI', Inter, Roboto, 'Helvetica Neue', Arial, sans-serif; font-size: 12px; }
* { transform-box: fill-box; }
${css}
</style>
<rect x="0.5" y="0.5" width="${W - 1}" height="${h - 1}" rx="10" fill="${C.bg}" stroke="${C.edge}"/>
${bar}
${body}
</svg>
`;
  writeFileSync(new URL(`${name}.svg`, OUT), doc);
}

// Keys you already know: type into the file, then the usual chords.
{
  const keys = [['Ctrl', 'S', 'Saved'], ['Ctrl', 'C', 'Copied'], ['Ctrl', 'Z', 'Undone'], ['Ctrl', 'Q', 'Closed']];
  const typed = 'Just start typing';
  const col = (W - 32) / 4;
  const body = [
    `<clipPath id="typed"><rect class="reveal" x="16" y="44" width="${typed.length * CH + 2}" height="24"/></clipPath>`,
    `<g clip-path="url(#typed)">${t(16, 61, typed, `fill="${C.fg}"`)}</g>`,
    `<rect class="caret" x="17" y="47" width="2" height="18" fill="${C.blue}"/>`,
    ...keys.map(([m, k, said], i) => {
      const cx = 16 + col * i + col / 2;
      const mw = 4 * CH + 14, kw = CH + 14, gap = 6;
      const x0 = cx - (mw + gap + kw) / 2;
      const cap = (x, w, s) =>
        `<g class="cap k${i}"><rect x="${x}" y="96" width="${w}" height="24" rx="5"/>${t(x + 7, 113, s, 'text-anchor="start"')}</g>`;
      return (
        cap(x0, mw, m) +
        cap(x0 + mw + gap, kw, k) +
        `<text class="said k${i} sans" x="${cx}" y="142" text-anchor="middle" fill="${C.blue}">${said}</text>`
      );
    }),
  ].join('\n');
  const css = `
.reveal { transform-origin: left; animation: type 10s steps(${typed.length}, end) infinite; }
.caret { animation: caret 10s steps(${typed.length}, end) infinite, blink 1s steps(1) infinite; }
.cap rect { fill: ${C.cap}; } .cap text { fill: ${C.capfg}; }
.cap { animation: press 10s infinite; }
.cap rect { animation: capbg 10s infinite; } .cap text { animation: capfg 10s infinite; }
.said { opacity: 0; animation: say 10s infinite; }
${keys.map((_, i) => `.k${i}, .k${i} rect, .k${i} text { animation-delay: ${2 + i * 2}s; }`).join('\n')}
@keyframes type { 0% { transform: scaleX(0); } 18%, 100% { transform: scaleX(1); } }
@keyframes caret { 0% { transform: translateX(0); } 18%, 100% { transform: translateX(${(typed.length * CH).toFixed(1)}px); } }
@keyframes blink { 50% { opacity: 0; } }
@keyframes press { 0% { transform: none; } 2%, 10% { transform: translateY(2px); } 14%, 100% { transform: none; } }
@keyframes capbg { 0% { fill: ${C.cap}; } 2%, 12% { fill: ${C.blue}; } 18%, 100% { fill: ${C.cap}; } }
@keyframes capfg { 0% { fill: ${C.capfg}; } 2%, 12% { fill: ${C.ink}; } 18%, 100% { fill: ${C.capfg}; } }
@keyframes say { 0% { opacity: 0; } 3%, 16% { opacity: 1; } 20%, 100% { opacity: 0; } }
@media (prefers-reduced-motion: reduce) { * { animation: none !important; } .said { opacity: 1; } }`;
  svg('keys', 156, 'Typing straight into a file, then Ctrl+S saves, Ctrl+C copies, Ctrl+Z undoes and Ctrl+Q quits.', 'notes.md', css, body);
}

// Everything is a tab: one tab bar, five kinds of tab.
{
  const tabs = ['main.rs', 'zsh', 'diff', 'claude', 'help'];
  const tw = W / tabs.length;
  const panes = [
    [[['fn', C.purple], [' ', ''], ['main', C.blue], ['() {', C.fg]], [['  ', ''], ['println!', C.blue], ['(', C.fg], ['"hi"', C.green], [');', C.fg]], [['}', C.fg]]],
    [[['$', C.cyan], [' cargo test', C.fg]], [['test result: ok. 42 passed', C.green]], [['$', C.cyan]]],
    [[['- let port = 80;', C.red]], [['+ let port = 8080;', C.green]], [['  serve(port);', C.dim]]],
    [[['>', C.cyan], [' add a --port flag', C.fg]], [['editing main.rs, cli.rs', C.dim]], [['2 files ready to review', C.green]]],
    [[['Ctrl+E', C.blue], ['  command line', C.fg]], [['Alt+P', C.blue], ['   open a file', C.fg]], [['F12', C.blue], ['     file / shell', C.fg]]],
  ];
  const body = [
    `<path d="M10 0.5h${W - 20}a9.5 9.5 0 0 1 9.5 9.5v22.5h-${W - 1}v-22.5a9.5 9.5 0 0 1 9.5-9.5z" fill="${C.bar}"/>`,
    ...tabs.map((s, i) => t(tw * i + tw / 2, 21, s, `fill="${C.bartext}" font-size="12" text-anchor="middle"`, s.length * 0.92)),
    `<rect class="mark" x="0.5" y="31" width="${tw}" height="2" fill="${C.blue}"/>`,
    ...panes.map(
      (rows, i) =>
        `<g class="pane p${i}">${rows.map((r, j) => line(16, 60 + j * 21, r)).join('')}${
          i === 1 ? `<rect class="blink" x="${16 + 2 * CH}" y="${60 + 2 * 21 - 11}" width="${CH}" height="14" fill="${C.fg}"/>` : ''
        }</g>`,
    ),
  ].join('\n');
  const step = (n) => `${(n * tw).toFixed(1)}px`;
  const css = `
.pane { opacity: 0; animation: show 12.5s infinite; }
${panes.map((_, i) => `.p${i} { animation-delay: ${i * 2.5}s; }`).join('\n')}
.mark { animation: slide 12.5s infinite; }
.blink { animation: blink 1s steps(1) infinite; }
@keyframes show { 0% { opacity: 0; transform: translateY(4px); } 3%, 18% { opacity: 1; transform: none; } 21%, 100% { opacity: 0; } }
@keyframes slide { 0%, 18% { transform: none; } 20%, 38% { transform: translateX(${step(1)}); } 40%, 58% { transform: translateX(${step(2)}); } 60%, 78% { transform: translateX(${step(3)}); } 80%, 98% { transform: translateX(${step(4)}); } 100% { transform: none; } }
@keyframes blink { 50% { opacity: 0; } }
@media (prefers-reduced-motion: reduce) { * { animation: none !important; } .p0 { opacity: 1; } }`;
  svg('tabs', 120, 'One tab bar holding a file, a shell, a diff, an AI agent and the help.', null, css, body);
}

// The same editor over SSH: keys go out, the copy comes home (OSC 52).
{
  const h = 132, mid = 56, a = 132, b = W - 132;
  const laptop = (x) =>
    `<g transform="translate(${x - 28} 24)" fill="none" stroke="${C.capfg}" stroke-width="2"><rect x="6" y="3" width="44" height="28" rx="3"/><path d="M1 37h54" stroke-linecap="round"/></g>`;
  const server = (x) =>
    `<g transform="translate(${x - 24} 22)" fill="none" stroke="${C.capfg}" stroke-width="2"><rect x="4" y="2" width="40" height="16" rx="2"/><rect x="4" y="22" width="40" height="16" rx="2"/><circle cx="11" cy="10" r="1.8" fill="${C.capfg}" stroke="none"/><circle cx="11" cy="30" r="1.8" fill="${C.capfg}" stroke="none"/></g>`;
  const chip = (cls, x, y, s, bg, fg) =>
    `<g class="${cls}"><rect x="${x}" y="${y}" width="${s.length * CH + 16}" height="22" rx="5" fill="${bg}"/>${t(x + 8, y + 15, s, `fill="${fg}" font-size="12"`)}</g>`;
  const outW = 3 * CH + 16, backW = 11 * CH + 16;
  const body = [
    laptop(66),
    server(W - 66),
    `<text class="sans" x="66" y="${h - 30}" text-anchor="middle" fill="${C.fg}">Your laptop</text>`,
    `<text class="sans" x="${W - 66}" y="${h - 30}" text-anchor="middle" fill="${C.fg}">Server running Skink</text>`,
    `<path d="M${a} ${mid}H${b}" stroke="${C.dim}" stroke-width="2" stroke-dasharray="6 6"/>`,
    chip('out', a, mid - 30, 'ssh', C.cap, C.fg),
    chip('back', a, mid + 8, 'copied text', C.blue, C.ink),
  ].join('\n');
  const css = `
.out { opacity: 0; animation: out 6s infinite ease-in-out; }
.back { opacity: 0; animation: back 6s infinite ease-in-out; }
@keyframes out { 0% { transform: none; opacity: 0; } 8% { opacity: 1; } 40% { transform: translateX(${b - a - outW}px); opacity: 1; } 48%, 100% { transform: translateX(${b - a - outW}px); opacity: 0; } }
@keyframes back { 0%, 50% { transform: translateX(${b - a - backW}px); opacity: 0; } 58% { opacity: 1; } 90% { transform: none; opacity: 1; } 98%, 100% { transform: none; opacity: 0; } }
@media (prefers-reduced-motion: reduce) { * { animation: none !important; } .out, .back { opacity: 1; } }`;
  svg('ssh', h, "Your laptop runs ssh to a server where Skink runs; text you copy on the server lands on your laptop's clipboard.", null, css, body);
}

// Bring your own agent: its edit arrives as an inline diff, y keeps it.
{
  const L = 21, y0 = 58, x = 16;
  const row = (y, cls, fill) => `<rect class="${cls}" x="1" y="${y - 15}" width="${W - 2}" height="${L}" fill="${fill}"/>`;
  const body = [
    t(x, y0, 'fn serve() {', `fill="${C.dim}"`),
    `<g class="old">${row(y0 + L, 'oldbg', C.red)}${t(x + 2 * CH, y0 + L, 'let port = 80;', 'class="oldtx"')}<rect class="strike" x="${x + 2 * CH}" y="${y0 + L - 5}" width="${14 * CH}" height="1.5" fill="${C.red}"/></g>`,
    `<g class="new">${row(y0 + 2 * L, 'newbg', C.green)}${row(y0 + 3 * L, 'newbg', C.green)}${t(x + 2 * CH, y0 + 2 * L, 'let port = env_port()', 'class="newtx"')}${t(x + 4 * CH, y0 + 3 * L, '.unwrap_or(8080);', 'class="newtx"')}</g>`,
    `<g class="close">${t(x, y0 + 4 * L, '}', `fill="${C.dim}"`)}</g>`,
    `<g class="ycap"><rect x="${x}" y="${y0 + 5 * L + 2}" width="${CH + 12}" height="20" rx="4"/>${t(x + 6, y0 + 5 * L + 16, 'y')}</g>`,
    `<text class="sans" x="${x + CH + 20}" y="${y0 + 5 * L + 16}" fill="${C.dim}">keep</text>`,
    `<rect x="${x + 64}" y="${y0 + 5 * L + 2}" width="${CH + 12}" height="20" rx="4" fill="${C.cap}"/>${t(x + 70, y0 + 5 * L + 16, 'n', `fill="${C.capfg}"`)}`,
    `<text class="sans" x="${x + CH + 84}" y="${y0 + 5 * L + 16}" fill="${C.dim}">drop</text>`,
  ].join('\n');
  const css = `
.oldtx { animation: oldtx 8s infinite; } .oldbg { opacity: 0; animation: oldbg 8s infinite; }
.strike { opacity: 0; animation: strike 8s infinite; }
.old { animation: oldout 8s infinite; }
.new { opacity: 0; animation: newin 8s infinite; }
.newtx { animation: newtx 8s infinite; } .newbg { animation: newbg 8s infinite; }
.close { animation: close 8s infinite; }
.ycap rect { animation: capbg 8s infinite; } .ycap text { animation: capfg 8s infinite; }
.ycap { animation: press 8s infinite; }
@keyframes oldtx { 0%, 15% { fill: ${C.fg}; } 20%, 100% { fill: ${C.red}; } }
@keyframes oldbg { 0%, 15% { opacity: 0; } 20%, 55% { opacity: 0.12; } 62%, 100% { opacity: 0; } }
@keyframes strike { 0%, 15% { opacity: 0; } 20%, 100% { opacity: 1; } }
@keyframes oldout { 0%, 55% { opacity: 1; } 62%, 100% { opacity: 0; } }
@keyframes newin { 0%, 22% { opacity: 0; transform: none; } 28%, 55% { opacity: 1; transform: none; } 62%, 100% { opacity: 1; transform: translateY(-${L}px); } }
@keyframes newtx { 0%, 55% { fill: ${C.green}; } 62%, 100% { fill: ${C.fg}; } }
@keyframes newbg { 0%, 55% { opacity: 0.12; } 62%, 100% { opacity: 0; } }
@keyframes close { 0%, 22% { transform: translateY(-${2 * L}px); } 28%, 55% { transform: none; } 62%, 100% { transform: translateY(-${L}px); } }
@keyframes capbg { 0%, 52% { fill: ${C.cap}; } 55%, 62% { fill: ${C.blue}; } 66%, 100% { fill: ${C.cap}; } }
@keyframes capfg { 0%, 52% { fill: ${C.capfg}; } 55%, 62% { fill: ${C.ink}; } 66%, 100% { fill: ${C.capfg}; } }
@keyframes press { 0%, 52% { transform: none; } 55%, 62% { transform: translateY(1px); } 66%, 100% { transform: none; } }
@media (prefers-reduced-motion: reduce) { * { animation: none !important; } .new { opacity: 1; } .oldtx { fill: ${C.red}; } .strike { opacity: 1; } }`;
  svg('agent', y0 + 5 * L + 36, "An AI agent's edit appears as an inline diff in the file; pressing y keeps the change.", 'server.rs', css, body);
}
