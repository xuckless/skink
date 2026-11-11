// The old site lived at https://xuckless.github.io/skink/. GitHub Pages cannot
// send real redirects, so this writes a tiny page for every old URL that
// points search engines (canonical) and people (refresh) at skink.cc.
// Usage: node scripts/gh-redirects.mjs <built dist> <out dir>
import { readdirSync, statSync, mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const [dist = 'dist', out = 'gh-redirects'] = process.argv.slice(2);
const moves = JSON.parse(readFileSync(new URL('../src/data/moves.json', import.meta.url), 'utf8'));
const NEW = 'https://skink.cc';

const page = (to) => `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>Skink has moved to skink.cc</title>
<link rel="canonical" href="${to}">
<meta http-equiv="refresh" content="0; url=${to}">
<script>location.replace(${JSON.stringify(to)} + location.hash)</script>
</head><body><p>Skink's site is now at <a href="${to}">${to}</a>.</p></body></html>
`;
const write = (path, to) => {
  const dir = join(out, path);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'index.html'), page(to));
};

const walk = (d) => readdirSync(d).flatMap((f) => (statSync(join(d, f)).isDirectory() ? walk(join(d, f)) : [join(d, f)]));
for (const f of walk(dist)) {
  if (!f.endsWith('/index.html')) continue;
  const path = relative(dist, f).replace(/index\.html$/, '');
  write(path, `${NEW}/${path}`);
}
for (const [from, to] of Object.entries(moves)) write(`${from}/`, `${NEW}/${to}/`);
// The one-page manual's anchors (#git) now have their own pages.
writeFileSync(
  join(out, 'reference/manual/index.html'),
  page(`${NEW}/reference/manual/`).replace(
    '<script>location.replace(',
    '<script>var h=location.hash.slice(1);if(h)location.replace("' + NEW + '/reference/manual/"+h+"/");else location.replace(',
  ),
);
writeFileSync(
  join(out, '404.html'),
  page(`${NEW}/`).replace('<script>location.replace(' + JSON.stringify(`${NEW}/`) + ' + location.hash)', '<script>location.replace("' + NEW + '"+location.pathname.replace(/^\\/skink/,"")+location.hash)'),
);
console.log('gh-redirects: wrote', out);
