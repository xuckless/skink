// Pulls Skink's real key bindings, commands, actions and manual out of the
// Rust sources so the docs never drift from the editor. Run before every
// build (npm run build does it). Output: src/data/reference.json.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const read = (p) => readFileSync(join(repo, p), 'utf8');

const unescape = (s) => s.replace(/\\"/g, '"').replace(/\\\\/g, '\\');
const STR = '"((?:[^"\\\\]|\\\\.)*)"';

// --- default bindings (skink-config) -------------------------------------
const cfg = read('crates/skink-config/src/lib.rs');
const tableStart = cfg.indexOf('pub const DEFAULT_BINDINGS');
const table = cfg.slice(tableStart, cfg.indexOf('];', tableStart));
const bindings = [];
for (const m of table.matchAll(new RegExp(`\\(\\s*${STR},\\s*${STR},?\\s*\\)`, 'g'))) {
  bindings.push({ raw: unescape(m[1]), action: unescape(m[2]) });
}

// Micro key names ("CtrlShiftRight", "Alt-CtrlH", "Ctrl-s") -> "Ctrl+Shift+→".
const ARROWS = { Up: '↑', Down: '↓', Left: '←', Right: '→' };
function prettyKey(raw) {
  let rest = raw;
  const mods = [];
  for (;;) {
    const m = rest.match(/^(Ctrl|Alt|Shift)-?/);
    if (!m || rest.length === m[0].length) break;
    mods.push(m[1]);
    rest = rest.slice(m[0].length);
  }
  let key = ARROWS[rest] ?? rest;
  // Micro: "Alt-N" (capital) is Alt+Shift+N; "Alt-n" is Alt+N.
  if (key.length === 1 && /[A-Z]/.test(key) && !mods.includes('Shift')) mods.push('Shift');
  if (key.length === 1) key = key.toUpperCase();
  if (key === 'Backtab') return 'Shift+Tab';
  const order = ['Ctrl', 'Alt', 'Shift'];
  mods.sort((a, b) => order.indexOf(a) - order.indexOf(b));
  return [...mods, key].join('+');
}

// --- editor actions (skink-core registry) --------------------------------
const core = read('crates/skink-core/src/command.rs');
const actions = new Map();
for (const m of core.matchAll(new RegExp(`add\\(\\s*${STR},\\s*${STR}`, 'g'))) {
  actions.set(unescape(m[1]), { name: unescape(m[1]), about: unescape(m[2]), kind: 'editor' });
}

// --- app actions + commands (skink-ui) ------------------------------------
const ui = read('crates/skink-ui/src/commands.rs');
const appStart = ui.indexOf('pub static APP_ACTIONS');
for (const m of ui.slice(appStart).matchAll(new RegExp(`name:\\s*${STR},\\s*about:\\s*${STR}`, 'g'))) {
  actions.set(unescape(m[1]), { name: unescape(m[1]), about: unescape(m[2]), kind: 'app' });
}
const cmdStart = ui.indexOf('pub static COMMANDS');
const commands = [];
const cmdRe = new RegExp(
  `name:\\s*${STR},\\s*aliases:\\s*&\\[([^\\]]*)\\],\\s*args:\\s*${STR},\\s*about:\\s*${STR}`,
  'g',
);
for (const m of ui.slice(cmdStart, appStart).matchAll(cmdRe)) {
  const aliases = [...m[2].matchAll(new RegExp(STR, 'g'))].map((a) => unescape(a[1]));
  commands.push({ name: unescape(m[1]), aliases, args: unescape(m[3]), about: unescape(m[4]) });
}

const describe = (chain) => {
  const parts = chain.split(/[|,]/).map((p) => p.trim());
  if (parts[0].startsWith('command-edit:')) {
    const pre = parts[0].slice(13).trim();
    return pre ? `Opens the command line with \`${pre}\`` : 'Opens the command line';
  }
  if (parts[0].startsWith('command:')) return `Runs \`:${parts[0].slice(8).trim()}\``;
  const found = parts.map((p) => actions.get(p)?.about).filter(Boolean);
  return found[0] ?? parts[0].replace(/([a-z])([A-Z])/g, '$1 $2');
};

const keys = bindings.map((b) => ({ ...b, key: prettyKey(b.raw), about: describe(b.action) }));

// --- manual (help.txt) ------------------------------------------------------
const help = read('crates/skink-ui/src/help.txt');
const manual = [];
for (const chunk of help.split(/^## /m).slice(1)) {
  const nl = chunk.indexOf('\n');
  const heading = chunk.slice(0, nl).trim();
  const body = chunk
    .slice(nl + 1)
    .replace(/\s+$/, '')
    .split('\n')
    .map((l) => l.replace(/^ {2}/, ''))
    .join('\n');
  const slug = heading.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  manual.push({ heading, slug, body });
}

const version = read('Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? '';

const out = join(here, '..', 'src', 'data', 'reference.json');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(
  out,
  JSON.stringify({ version, keys, commands, actions: [...actions.values()], manual }, null, 1),
);
console.log(
  `extract: ${keys.length} keys, ${commands.length} commands, ${actions.size} actions, ${manual.length} manual sections (v${version})`,
);
