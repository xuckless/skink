// Tells IndexNow (Bing, Yandex, Seznam, Naver...) which URLs a deploy added, changed or removed.
//   node scripts/indexnow.mjs before            saves the live sitemap (run before deploying)
//   node scripts/indexnow.mjs after             compares it with dist/ and submits the difference
// Changed = new <loc>, a different <lastmod>, or a <loc> that is gone (Bing then sees its 404 or 301).
// The key file is public/<key>.txt; IndexNow fetches it to check that we own the host.
import { readFileSync, writeFileSync, readdirSync, existsSync } from 'node:fs';

const host = 'skink.cc';
const saved = 'indexnow-before.json';
const key = readdirSync('public').find((f) => /^[0-9a-f]{32}\.txt$/.test(f))?.slice(0, -4);
if (!key) throw new Error('no IndexNow key file in public/');

const parse = (xml) =>
  Object.fromEntries([...xml.matchAll(/<url><loc>(.*?)<\/loc>(?:<lastmod>(.*?)<\/lastmod>)?/g)].map((m) => [m[1], m[2] ?? '']));

async function live() {
  const index = await (await fetch(`https://${host}/sitemap-index.xml`)).text();
  const maps = [...index.matchAll(/<loc>(.*?)<\/loc>/g)].map((m) => m[1]);
  const urls = {};
  for (const m of maps) Object.assign(urls, parse(await (await fetch(m)).text()));
  return urls;
}

function built() {
  const urls = {};
  for (const f of readdirSync('dist').filter((f) => /^sitemap-\d+\.xml$/.test(f)))
    Object.assign(urls, parse(readFileSync(`dist/${f}`, 'utf8')));
  return urls;
}

if (process.argv[2] === 'before') {
  let urls = {};
  try {
    urls = await live();
  } catch (e) {
    console.warn(`indexnow: could not read the live sitemap (${e.message}); submitting nothing`);
    urls = null;
  }
  writeFileSync(saved, JSON.stringify(urls));
} else if (process.argv[2] === 'after') {
  const before = existsSync(saved) ? JSON.parse(readFileSync(saved, 'utf8')) : null;
  if (!before) process.exit(0);
  const now = built();
  const changed = [
    ...Object.keys(now).filter((u) => before[u] !== now[u]),
    ...Object.keys(before).filter((u) => !(u in now)),
  ];
  if (!changed.length) {
    console.log('indexnow: nothing changed');
    process.exit(0);
  }
  if (process.env.INDEXNOW_DRY) {
    console.log(changed.join('\n'));
    process.exit(0);
  }
  const res = await fetch('https://api.indexnow.org/indexnow', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json; charset=utf-8' },
    body: JSON.stringify({ host, key, keyLocation: `https://${host}/${key}.txt`, urlList: changed.slice(0, 10000) }),
  });
  console.log(`indexnow: submitted ${changed.length} URL(s): HTTP ${res.status}`);
  for (const u of changed) console.log(`  ${u}`);
  // 200/202 = accepted. Anything else is reported but never fails the deploy.
} else {
  throw new Error('usage: indexnow.mjs before|after');
}
