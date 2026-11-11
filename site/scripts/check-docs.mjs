// Fails the build when a docs page mentions a `:command` the editor does not
// know, so a renamed command (:forward -> :port) cannot silently rot a page.
// What counts as known mirrors App::run_command: command names and aliases
// (any case), action names (case, '-' and '_' ignored), the short names, and
// `:40` / `:40:5` line jumps. Run after extract.mjs.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const ref = JSON.parse(readFileSync(new URL('../src/data/reference.json', import.meta.url)));
const loose = (s) => s.replace(/[-_]/g, '').toLowerCase();

const known = new Set();
for (const c of ref.commands) for (const n of [c.name, ...c.aliases]) known.add(loose(n));
for (const a of ref.actions) known.add(loose(a.name));
for (const g of ref.shortGroups) for (const n of g.names) known.add(loose(typeof n === 'string' ? n : n.name));

// Vim's own commands, which the compare and article pages about Vim quote.
// Allowed there only: on a how-to page they would be a wrong instruction.
const VIM = new Set(['wq', 'wqa', 'x', 'q!', 'qa!', 'w!', 'version', 'm', 'cnext', 'checktime', 's', 'sort']);
const vimPage = (f) => /^(compare|articles)\//.test(f);

// Per page: names it may show that are not commands, and why.
const DEFINED = new Map([
  ['customize/scripting.mdx', new Set(['reviewed', 'name'])], // a script's own command; the placeholder
  ['start/exit-skink.mdx', new Set(['wq', 'q!'])], // "there is no :wq or :q!"
]);

const root = new URL('../src/content/docs/', import.meta.url).pathname;
const files = [];
const walk = (d) => {
  for (const f of readdirSync(d)) {
    const p = join(d, f);
    if (statSync(p).isDirectory()) walk(p);
    else if (p.endsWith('.mdx') || p.endsWith('.md')) files.push(p);
  }
};
walk(root);

// A command is written as inline code starting with ':' (`:help themes`) or
// as a code-block line starting with ':'.
const bad = [];
for (const f of files) {
  const text = readFileSync(f, 'utf8');
  const lines = text.split('\n');
  let fence = false;
  lines.forEach((line, i) => {
    if (/^\s*```/.test(line)) fence = !fence;
    const hits = fence
      ? [...line.matchAll(/^\s*:([A-Za-z][\w!-]*)/g)]
      : [...line.matchAll(/`:([A-Za-z][\w!-]*)[^`]*`/g)];
    for (const m of hits) {
      const name = m[1];
      const page = relative(root, f);
      if (known.has(loose(name)) || (vimPage(page) && VIM.has(name)) || DEFINED.get(page)?.has(name)) continue;
      bad.push(`${page}:${i + 1}: :${name}`);
    }
  });
}

if (bad.length) {
  console.error(`check-docs: ${bad.length} unknown command(s):\n  ${bad.join('\n  ')}`);
  process.exit(1);
}
console.log(`check-docs: ${files.length} pages, every :command is known`);
