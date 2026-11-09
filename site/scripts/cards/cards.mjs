// Renders the share cards:
//   site/public/og.png               1200x630  the site's og:image
//   docs/readme/social-preview.png   1280x640  GitHub repo social preview (upload by hand:
//                                              Settings > General > Social preview)
// Stars, forks and issues come live from the GitHub API; a count only shows
// once it is above zero, real editor numbers fill the row until then.
//
//   cd site && npm run cards      (needs playwright-core and a Chromium; set CHROMIUM=path)
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { pixelSvg } from './pixel.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const site = join(here, '..', '..');
const repo = join(site, '..');
const version = readFileSync(join(repo, 'Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)[1];

let gh = { stargazers_count: 0, forks_count: 0, open_issues_count: 0 };
try {
  const r = await fetch('https://api.github.com/repos/xuckless/skink', { headers: { 'User-Agent': 'skink-cards' } });
  if (r.ok) gh = await r.json();
} catch {}
const fmt = (n) => (n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + 'k' : String(n));

const ICON = {
  dot: '<circle cx="8" cy="8" r="6" fill="#dea584"/>',
  tag: '<path d="M2 2h6l6 6-6 6-6-6z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/><circle cx="5.5" cy="5.5" r="1.2" fill="currentColor"/>',
  law: '<path d="M8 2v12M4 14h8M3 4h10M3 4l-2 5a2 2 0 0 0 4 0zM13 4l-2 5a2 2 0 0 0 4 0z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/>',
  star: '<path d="M8 1.5l2 4.2 4.6.6-3.4 3.2.9 4.6L8 11.9 3.9 14.1l.9-4.6L1.4 6.3 6 5.7z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>',
  fork: '<circle cx="4" cy="3" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"/><circle cx="12" cy="3" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"/><circle cx="8" cy="13" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M4 4.6v1.4a2 2 0 0 0 2 2h4a2 2 0 0 0 2-2V4.6M8 8v3.4" fill="none" stroke="currentColor" stroke-width="1.4"/>',
  issue: '<circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.5"/><circle cx="8" cy="8" r="1.4" fill="currentColor"/>',
  chip: '<rect x="3" y="3" width="10" height="10" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M6 1v2M10 1v2M6 13v2M10 13v2M1 6h2M1 10h2M13 6h2M13 10h2" stroke="currentColor" stroke-width="1.3"/>',
  globe: '<circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12" fill="none" stroke="currentColor" stroke-width="1.2"/>',
  key: '<rect x="1.5" y="4" width="13" height="8" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M4 7h1M7 7h2M11 7h1M5 9.5h6" stroke="currentColor" stroke-width="1.3"/>',
};
const icon = (k, s) => `<svg viewBox="0 0 16 16" width="${s}" height="${s}" style="flex:none">${ICON[k]}</svg>`;

const stats = [
  ['dot', 'Rust'],
  ['tag', `v${version}`],
  ['law', 'MIT / Apache-2.0'],
];
for (const [k, n, label] of [
  ['star', gh.stargazers_count, 'stars'],
  ['fork', gh.forks_count, 'forks'],
  ['issue', gh.open_issues_count, 'issues'],
]) {
  if (n > 0) stats.push([k, `<b>${fmt(n)}</b> ${n === 1 ? label.slice(0, -1) : label}`]);
}
for (const extra of [['chip', '<b>8 MB</b> idle RAM'], ['globe', '<b>19</b> languages'], ['key', '<b>0</b> modes']]) {
  if (stats.length < 5) stats.push(extra);
}

const FONTS =
  '<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;800&family=JetBrains+Mono:wght@500&display=block" rel="stylesheet">';
const BASE_CSS = `*{box-sizing:border-box}body{margin:0;background:#16161e;color:#c0caf5;font-family:Inter,system-ui,sans-serif;-webkit-font-smoothing:antialiased}
.bg{position:absolute;inset:0;background:
 radial-gradient(900px 500px at 85% -10%,rgba(122,162,247,.18),transparent 60%),
 radial-gradient(700px 500px at -10% 110%,rgba(115,218,202,.13),transparent 60%),
 radial-gradient(circle at 1px 1px,rgba(192,202,245,.07) 1px,transparent 0) 0 0/28px 28px}
.bar{position:absolute;left:0;right:0;bottom:0;height:14px;background:linear-gradient(90deg,#73daca,#7aa2f7 55%,#bb9af7)}
.grad{background:linear-gradient(120deg,#73daca,#7aa2f7 55%,#bb9af7);-webkit-background-clip:text;color:transparent}`;

const githubCard = `<!doctype html><html><head><meta charset="utf-8">${FONTS}<style>${BASE_CSS}
.card{position:relative;width:1280px;height:640px;overflow:hidden}
.main{position:absolute;left:84px;top:78px;width:780px}
.owner{font-size:46px;color:#737aa2;font-weight:500;letter-spacing:-1px}
.name{font-size:116px;font-weight:800;letter-spacing:-5px;line-height:1;margin-top:2px}
.desc{font-size:31px;line-height:1.38;color:#a9b1d6;margin-top:30px;max-width:740px}
.desc b{color:#c0caf5;font-weight:600}
.term{display:inline-flex;gap:14px;margin-top:30px;font:500 20px 'JetBrains Mono',monospace;color:#a9b1d6;padding:12px 18px;border:1px solid #292e42;border-radius:12px;background:rgba(26,27,38,.8)}
.term .p{color:#73daca}
.avatar{position:absolute;right:84px;top:84px;filter:drop-shadow(0 24px 40px rgba(0,0,0,.45))}
.stats{position:absolute;left:84px;right:84px;bottom:62px;display:flex;flex-wrap:wrap;gap:16px 40px;font-size:25px;color:#a9b1d6}
.stats div{display:flex;align-items:center;gap:11px}
.stats b{color:#c0caf5;font-weight:600}
</style></head><body><div class="card"><div class="bg"></div>
<div class="main"><div class="owner">xuckless/</div><div class="name grad">skink</div>
<div class="desc">A modeless terminal code editor. <b>Neovim’s reach</b>, <b>Micro’s keys</b>, <b>Zed’s features</b>, over SSH, with a seat for every AI agent.</div>
<div class="term"><span class="p">$</span><span>skink .</span><span style="color:#565f89">#  Ctrl+S saves · Ctrl+Q quits</span></div></div>
<div class="avatar">${pixelSvg(270)}</div>
<div class="stats">${stats.map(([k, t]) => `<div>${icon(k, 26)}<span>${t}</span></div>`).join('')}</div>
<div class="bar"></div></div></body></html>`;

const shot = readFileSync(join(site, 'public', 'screenshot.png')).toString('base64');
const logo = readFileSync(join(site, 'public', 'logo-dark.svg')).toString('base64');
const siteCard = `<!doctype html><html><head><meta charset="utf-8">${FONTS}<style>${BASE_CSS}
.card{position:relative;width:1200px;height:630px;overflow:hidden}
.left{position:absolute;left:72px;top:64px;width:560px}
.logo{height:58px}
h1{font-size:62px;line-height:1.02;letter-spacing:-2.6px;font-weight:800;margin:44px 0 0}
p{font-size:25px;line-height:1.45;color:#a9b1d6;margin:26px 0 0}
.pills{display:flex;flex-wrap:wrap;gap:10px;margin-top:30px}
.pills span{font-size:18px;font-weight:500;padding:8px 15px;border-radius:999px;border:1px solid #3b4261;color:#c0caf5;background:rgba(26,27,38,.85)}
.url{position:absolute;left:72px;bottom:46px;font:500 20px 'JetBrains Mono',monospace;color:#737aa2}
.shot{position:absolute;left:676px;top:72px;width:900px;border-radius:16px;border:1px solid #3b4261;box-shadow:0 30px 70px rgba(0,0,0,.55)}
.av{position:absolute;right:40px;bottom:40px;filter:drop-shadow(0 10px 24px rgba(0,0,0,.6))}
</style></head><body><div class="card"><div class="bg"></div>
<div class="left"><img class="logo" src="data:image/svg+xml;base64,${logo}">
<h1>The terminal editor that <span class="grad">doesn’t make you learn modes.</span></h1>
<p>Neovim’s reach, Micro’s keys, Zed’s features. Over SSH, next to your AI agent.</p>
<div class="pills"><span>Mouse &amp; trackpad</span><span>LSP</span><span>Git</span><span>AI agents</span></div></div>
<img class="shot" src="data:image/png;base64,${shot}">
<div class="url">xuckless.github.io/skink</div>
<div class="bar"></div></div></body></html>`;

const { chromium } = await import('playwright-core');
const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
for (const [html, w, h, out] of [
  [githubCard, 1280, 640, join(repo, 'docs', 'readme', 'social-preview.png')],
  [siteCard, 1200, 630, join(site, 'public', 'og.png')],
]) {
  const page = await browser.newPage({ viewport: { width: w, height: h } });
  await page.setContent(html, { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: out });
  await page.close();
  console.log(`cards: ${out}`);
}
writeFileSync(join(site, 'public', 'avatar.svg'), pixelSvg(256));
await browser.close();
console.log(`cards: stars ${gh.stargazers_count}, forks ${gh.forks_count}, issues ${gh.open_issues_count}`);
