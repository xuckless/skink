// The package index: every GitHub repo with the `skink-plugin` topic whose
// latest release has `<package>.skink.tar.gz` assets, read from the
// archives' skink-package.toml. Verified only when registry/verified.json
// says so (never the repo's own claim). Written to src/data/packages.json,
// which the /packages/ page and /packages/index.json (schema 1, read by
// `skink --search` and `skink --install`) are built from.
//
// Offline or rate-limited: falls back to the first-party release's
// index.json, then to verified.json alone, with a warning. Never fails the
// build. GITHUB_TOKEN raises the API limits when set.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';

const root = new URL('../../', import.meta.url);
const OUT = new URL('../src/data/packages.json', import.meta.url);
const API = 'https://api.github.com';
const FALLBACK_INDEX = 'https://github.com/xuckless/skink-packages/releases/latest/download/index.json';
const token = process.env.GITHUB_TOKEN;
const headers = { Accept: 'application/vnd.github+json', 'User-Agent': 'skink-site-index', ...(token && { Authorization: `Bearer ${token}` }) };

// ---- rules shared with the client (crates/skink-ui/src/packages.rs) ---------

const validName = (n) => typeof n === 'string' && n.length > 0 && n.length <= 64 && /^[a-z0-9-]+$/.test(n) && !n.startsWith('-') && !n.endsWith('-');
const ghPart = (s) => typeof s === 'string' && /^[A-Za-z0-9._-]+$/.test(s) && s !== '.' && s !== '..';
const validRepo = (r) => typeof r === 'string' && r.split('/').length === 2 && r.split('/').every(ghPart);
const FOLD = { 0: 'o', 1: 'l', i: 'l', 3: 'e', 4: 'a', 5: 's', 7: 't', 8: 'b', 9: 'g' };
export const fold = (name) =>
  [...name]
    .filter((c) => !'-_. '.includes(c))
    .map((c) => FOLD[c.toLowerCase()] ?? c.toLowerCase())
    .join('')
    .replaceAll('rn', 'm')
    .replaceAll('vv', 'w');

// ---- verified.json ---------------------------------------------------------

const verified = Object.entries(JSON.parse(readFileSync(new URL('registry/verified.json', root), 'utf8')).packages ?? {})
  .filter(([n, e]) => validName(n) && validRepo(e.repo))
  .map(([name, e]) => ({ name, ...e }));
const isVerified = (p) => verified.some((v) => v.name === p.name && v.repo === p.repo && (v.package ?? null) === (p.package ?? null));

// ---- a minimal skink-package.toml reader (tables, strings, arrays) ---------

function readManifest(src) {
  const tables = {};
  let cur = null;
  const lines = src.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    let l = lines[i].trim();
    if (!l || l.startsWith('#')) continue;
    const h = l.match(/^\[\[?\s*([A-Za-z0-9_.-]+)\s*\]\]?/);
    if (h) {
      const name = h[1].split('.')[0];
      if (l.startsWith('[[') && !h[1].includes('.')) (tables[name] ??= []).push((cur = {}));
      else if (!h[1].includes('.')) tables[name] = [(cur = {})];
      else cur = {}; // sub-table like [tool.targets]: not needed here
      continue;
    }
    const kv = l.match(/^([A-Za-z0-9_-]+)\s*=\s*(.*)$/);
    if (!kv || !cur) continue;
    let val = kv[2];
    // Arrays may span lines: read until the brackets balance.
    while (val.startsWith('[') && (val.match(/\[/g) || []).length > (val.match(/\]/g) || []).length && i + 1 < lines.length) val += ' ' + lines[++i].trim();
    cur[kv[1]] = val.startsWith('"') ? (val.match(/^"((?:[^"\\]|\\.)*)"/)?.[1] ?? '') : val.startsWith('[') ? [...val.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]) : val;
  }
  const pkg = tables.package?.[0];
  if (!pkg || !validName(pkg.name)) return null;
  const grammars = tables.grammar ?? [];
  const uniq = (a) => [...new Set(a)];
  return {
    name: pkg.name,
    version: pkg.version ?? '',
    description: pkg.description ?? '',
    license: pkg.license ?? '',
    extensions: uniq(grammars.flatMap((g) => g.extensions ?? [])),
    file_names: uniq(grammars.flatMap((g) => g['file-names'] ?? [])),
    kinds: ['grammar', 'lsp', 'tool', 'theme', 'font', 'script'].filter((k) => tables[k]?.length),
  };
}

/** skink-package.toml from a .tar.gz: at the top, or in its single top folder. */
function manifestFromTarGz(buf) {
  const tar = gunzipSync(buf);
  for (let off = 0; off + 512 <= tar.length; ) {
    const h = tar.subarray(off, off + 512);
    if (h.every((b) => b === 0)) break;
    const str = (a, b) => h.subarray(a, b).toString('utf8').replace(/\0.*$/s, '');
    const name = (str(345, 500) ? str(345, 500) + '/' : '') + str(0, 100);
    const size = parseInt(str(124, 136).trim() || '0', 8);
    const type = String.fromCharCode(h[156] || 48);
    const parts = name.replace(/^\.\//, '').split('/').filter(Boolean);
    if ((type === '0' || type === '\0') && parts.at(-1) === 'skink-package.toml' && parts.length <= 2)
      return tar.subarray(off + 512, off + 512 + size).toString('utf8');
    off += 512 + Math.ceil(size / 512) * 512;
  }
  return null;
}

// ---- GitHub ----------------------------------------------------------------

async function get(url, json = true) {
  const r = await fetch(url, { headers, signal: AbortSignal.timeout(20000), redirect: 'follow' });
  if (!r.ok) throw new Error(`${r.status} ${url}`);
  return json ? r.json() : Buffer.from(await r.arrayBuffer());
}

async function fromGitHub() {
  const repos = [];
  for (let page = 1; page <= 10; page++) {
    const r = await get(`${API}/search/repositories?q=topic:skink-plugin&per_page=100&page=${page}`);
    repos.push(...r.items);
    if (r.items.length < 100) break;
  }
  const out = [];
  for (const repo of repos.filter((r) => !r.archived && !r.fork)) {
    let rel;
    try {
      rel = await get(`${API}/repos/${repo.full_name}/releases/latest`);
    } catch {
      continue; // no release yet
    }
    const assets = rel.assets.filter((a) => a.name.endsWith('.skink.tar.gz'));
    for (const a of assets) {
      let m = null;
      try {
        m = readManifest(manifestFromTarGz(await get(a.browser_download_url, false)) ?? '');
      } catch (e) {
        console.warn(`build-index: ${repo.full_name}/${a.name}: ${e.message}`);
      }
      if (!m) continue;
      const assetName = a.name.replace(/\.skink\.tar\.gz$/, '');
      const single = assets.length === 1 && assetName === repo.name;
      out.push({ ...m, repo: repo.full_name, ...(single ? {} : { package: assetName }), stars: repo.stargazers_count, updated: rel.published_at });
    }
  }
  return out;
}

// ---- build -----------------------------------------------------------------

let packages, source;
try {
  packages = await fromGitHub();
  source = 'github';
} catch (e) {
  console.warn(`build-index: GitHub search failed (${e.message}); using the skink-packages release index.json`);
  try {
    const idx = await get(FALLBACK_INDEX);
    if (idx.schema !== 1) throw new Error('unknown schema');
    packages = idx.packages;
    source = 'release index.json';
  } catch (e2) {
    console.warn(`build-index: fallback failed too (${e2.message}); listing verified.json only`);
    packages = [];
    source = 'verified.json';
  }
}

packages = packages.filter((p) => validName(p.name) && validRepo(p.repo));
for (const p of packages) p.verified = isVerified(p);
// Verified entries the search did not find (no topic, or offline).
for (const v of verified)
  if (!packages.some((p) => p.verified && p.name === v.name))
    packages.push({ name: v.name, repo: v.repo, ...(v.package && { package: v.package }), description: v.description ?? '', version: '', verified: true, extensions: v.extensions ?? [], file_names: v.file_names ?? [], kinds: [] });
// Same name as a verified package, or a name that folds to one: flag it.
for (const p of packages) {
  if (p.verified) continue;
  const like = verified.find((v) => fold(v.name) === fold(p.name));
  if (like) p.lookalike_of = like.name;
}
packages.sort((a, b) => (b.verified - a.verified) || (a.verified ? a.name.localeCompare(b.name) : (b.stars ?? 0) - (a.stars ?? 0) || a.name.localeCompare(b.name)));

mkdirSync(new URL('.', OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify({ schema: 1, generated: new Date().toISOString().replace(/\.\d+Z$/, 'Z'), source, packages }, null, 1) + '\n');
console.log(`build-index: ${packages.length} packages (${packages.filter((p) => p.verified).length} verified) from ${source}`);
