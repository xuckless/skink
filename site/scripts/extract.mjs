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

// Typed command for each key: a port of command_for() in skink-ui/src/commands.rs.
const shortStart = ui.indexOf('pub static SHORT_NAMES');
const shortNames = shortStart < 0 ? [] : [...ui.slice(shortStart, ui.indexOf('];', shortStart)).matchAll(new RegExp(`\\(\\s*${STR},\\s*${STR}\\s*\\)`, 'g'))].map((m) => [unescape(m[1]), unescape(m[2])]);
function commandFor(chain) {
  if (chain.startsWith('command-edit:')) return null;
  const exact = shortNames.find(([, a]) => a === chain);
  if (exact) return exact[0];
  const first = chain.split(/[|,]/).map((p) => p.trim()).find((n) => n !== 'Autocomplete' && n !== 'CycleAutocompleteBack');
  if (!first) return null;
  const cmd = commands.find((c) => c.name === first.toLowerCase());
  if (cmd) return cmd.name;
  const short = shortNames.find(([, a]) => a.split('|')[0] === first);
  return short ? short[0] : first.toLowerCase();
}

const keys = bindings.map((b) => ({ ...b, key: prettyKey(b.raw), about: describe(b.action), typed: commandFor(b.action) }));
const shortGroups = [];
if (shortStart >= 0) {
  let group = null;
  for (const line of ui.slice(shortStart, ui.indexOf('];', shortStart)).split('\n')) {
    const c = line.match(/^\s*\/\/\s*(.+)$/);
    if (c) { group = { group: c[1].trim(), names: [] }; shortGroups.push(group); continue; }
    const m = line.match(new RegExp(`\\(\\s*${STR},\\s*${STR}\\s*\\)`));
    if (m && group) group.names.push({ name: unescape(m[1]), action: unescape(m[2]), about: describe(unescape(m[2])) });
  }
}

// --- manual (the :help pages) ----------------------------------------------
// One Markdown file per topic in crates/skink-ui/src/help/, in the order of
// TOPICS in help.rs. The landing page and the generated reference pages
// (keys, commands, actions) are left out: the site has its own for those.
// `body` is Markdown (without the page's `# Title` line).
const helpRs = read('crates/skink-ui/src/help.rs');
const manual = [];
for (const m of helpRs.matchAll(/topic!\(\s*"([^"]+)",\s*"((?:[^"\\]|\\.)*)"/g)) {
  const [, id, title] = m;
  if (['index', 'keys', 'commands', 'actions'].includes(id)) continue;
  const heading = unescape(title);
  const body = read(`crates/skink-ui/src/help/${id}.md`)
    .replace(/^# .*\n+/, '')
    .replace(/\s+$/, '');
  const slug = heading.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  manual.push({ id, heading, slug, body });
}

const version = read('Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? '';

const out = join(here, '..', 'src', 'data', 'reference.json');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(
  out,
  JSON.stringify({ version, keys, commands, actions: [...actions.values()], manual, shortGroups }, null, 1),
);
console.log(
  `extract: ${keys.length} keys, ${commands.length} commands, ${actions.size} actions, ${manual.length} manual sections (v${version})`,
);
