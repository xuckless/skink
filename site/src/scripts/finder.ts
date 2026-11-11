// "What would you like to search?" — a problem-language search over the docs,
// every default shortcut, every command and the built-in manual.
type Item = { t: string; k: string; u: string; a?: string; w?: string; x?: string; kb?: string };
type Hit = { item: Item; score: number };

const STOP = new Set(
  'a an and are can do does for from get how i in is it me my of on or skink the this to use using what when where which why with you your want would like please'.split(' '),
);

// Problem words -> words the docs use. Both directions are searched.
const SYNONYMS: string[][] = [
  ['exit', 'quit', 'close', 'leave'],
  ['bigger', 'larger', 'zoom', 'font', 'size', 'text', 'smaller', 'enlarge'],
  ['unhide', 'show', 'hidden', 'dotfiles', 'reveal', 'visible', 'hide'],
  ['sidebar', 'tree', 'explorer', 'files', 'panel'],
  ['ai', 'agent', 'agents', 'claude', 'gemini', 'codex', 'opencode', 'goose', 'llm', 'copilot', 'acp'],
  ['ssh', 'remote', 'server', 'mosh', 'tmux', 'headless'],
  ['copy', 'clipboard', 'paste', 'yank', 'osc'],
  ['split', 'pane', 'panes', 'side', 'vsplit', 'window'],
  ['terminal', 'shell', 'console', 'command', 'bash', 'zsh'],
  ['undo', 'revert', 'back'],
  ['save', 'write'],
  ['find', 'search', 'grep', 'look'],
  ['replace', 'substitute', 'sub'],
  ['rename', 'refactor', 'refactoring'],
  ['mouse', 'click', 'trackpad', 'scroll', 'gesture', 'gestures', 'touchpad', 'drag'],
  ['shortcut', 'shortcuts', 'keys', 'keybinding', 'keybindings', 'bindings', 'hotkey', 'remap', 'rebind'],
  ['settings', 'config', 'configuration', 'preferences', 'options', 'setup', 'set'],
  ['install', 'download', 'setup', 'upgrade', 'update', 'configure'],
  ['cursor', 'cursors', 'multi', 'multiple'],
  ['vim', 'neovim', 'nvim', 'vi'],
  ['lsp', 'language', 'server', 'completion', 'autocomplete', 'intellisense', 'definition'],
  ['git', 'commit', 'diff', 'blame', 'stage', 'hunk', 'branch'],
  ['tab', 'tabs', 'buffer', 'buffers'],
  ['fold', 'folding', 'collapse'],
  ['mac', 'macos', 'option', 'alt', 'meta'],
];
const SYN = new Map<string, string[]>();
for (const g of SYNONYMS) for (const w of g) SYN.set(w, [...(SYN.get(w) ?? []), ...g.filter((x) => x !== w)]);

const words = (s: string) => s.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);

function near(a: string, b: string) {
  // one edit apart (typos), only for words long enough to be unambiguous
  if (Math.abs(a.length - b.length) > 1 || a.length < 5) return false;
  let i = 0, j = 0, edits = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) { i++; j++; continue; }
    if (++edits > 1) return false;
    if (a.length > b.length) i++;
    else if (b.length > a.length) j++;
    else { i++; j++; }
  }
  return edits + (a.length - i) + (b.length - j) <= 1;
}

type Indexed = { item: Item; f: [Set<string>, number][] };

function prepare(items: Item[]): Indexed[] {
  return items.map((item) => ({
    item,
    f: [
      [new Set(words(item.t)), 4],
      [new Set(words(item.w ?? '')), 3],
      [new Set(words(item.kb ?? '')), 3],
      [new Set(words(item.a ?? '')), 1.6],
      [new Set(words(item.x ?? '')), 0.6],
    ],
  }));
}

function matchField(set: Set<string>, q: string) {
  if (set.has(q)) return 1;
  let best = 0;
  for (const w of set) {
    if (q.length >= 3 && w.startsWith(q)) best = Math.max(best, 0.75);
    else if (near(w, q)) best = Math.max(best, 0.5);
  }
  return best;
}

const KIND_BOOST: Record<string, number> = { start: 1.25, guide: 1.15, key: 1.05, command: 1, article: 0.95, compare: 0.9, reference: 1, manual: 0.85 };

function search(index: Indexed[], query: string): Hit[] {
  const raw = words(query.toLowerCase().replace(/\bset\s+up\b/g, 'setup').replace(/\bsign\s+in\b/g, 'login'));
  const terms = raw.filter((w) => !STOP.has(w));
  const qs = terms.length ? terms : raw;
  if (!qs.length) return [];
  const hits: Hit[] = [];
  for (const d of index) {
    let score = 0;
    let matched = 0;
    for (const q of qs) {
      let best = 0;
      for (const [set, weight] of d.f) best = Math.max(best, matchField(set, q) * weight);
      for (const s of SYN.get(q) ?? []) for (const [set, weight] of d.f) best = Math.max(best, (set.has(s) ? 0.55 : 0) * weight);
      if (best > 0) matched++;
      score += best;
    }
    if (!matched) continue;
    score *= (matched / qs.length) ** 2 * (KIND_BOOST[d.item.k] ?? 1);
    if (d.item.t.toLowerCase().includes(query.trim().toLowerCase())) score += 3;
    hits.push({ item: d.item, score });
  }
  hits.sort((a, b) => b.score - a.score);
  // keep the list varied: at most 4 bare shortcuts / manual chunks
  const out: Hit[] = [];
  const per: Record<string, number> = {};
  for (const h of hits) {
    const cap = h.item.k === 'key' || h.item.k === 'manual' || h.item.k === 'command' ? 4 : 8;
    if ((per[h.item.k] = (per[h.item.k] ?? 0) + 1) > cap) continue;
    out.push(h);
    if (out.length >= 9) break;
  }
  return out;
}

const LABEL: Record<string, string> = {
  start: 'Start', guide: 'Guide', key: 'Shortcut', command: 'Command', article: 'Article', compare: 'Compare', reference: 'Reference', manual: 'Manual',
};

let indexPromise: Promise<Indexed[]> | null = null;
const loadIndex = () =>
  (indexPromise ??= fetch('/search.json').then((r) => r.json()).then(prepare));

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
const kbd = (k: string) =>
  k.split('+').map((p) => `<kbd>${esc(p)}</kbd>`).join('<span class="plus">+</span>');

function render(list: HTMLElement, hits: Hit[], q: string) {
  if (!q.trim()) { list.innerHTML = ''; list.hidden = true; return; }
  list.hidden = false;
  if (!hits.length) {
    list.innerHTML = `<li class="empty">Nothing for “${esc(q)}”. Try other words, or <a href="/reference/manual/">read the manual</a>.</li>`;
    return;
  }
  list.innerHTML = hits
    .map(({ item }, i) => `<li role="option" id="fr-${i}" aria-selected="${i === 0}">
      <a href="${esc(item.u)}">
        <span class="kind k-${item.k}">${LABEL[item.k] ?? item.k}</span>
        <span class="body"><span class="title">${esc(item.t)}</span>${item.kb ? `<span class="keys">${kbd(item.kb)}</span>` : ''}
        ${item.a ? `<span class="ans">${esc(item.a)}</span>` : ''}</span>
      </a></li>`)
    .join('');
}

function wire(root: HTMLElement) {
  const input = root.querySelector<HTMLInputElement>('input')!;
  const list = root.querySelector<HTMLElement>('[data-results]')!;
  let sel = 0;
  const move = (d: number) => {
    const opts = [...list.querySelectorAll<HTMLElement>('[role=option]')];
    if (!opts.length) return;
    sel = (sel + d + opts.length) % opts.length;
    opts.forEach((o, i) => o.setAttribute('aria-selected', String(i === sel)));
    opts[sel].scrollIntoView({ block: 'nearest' });
    input.setAttribute('aria-activedescendant', opts[sel].id);
  };
  const run = async () => {
    const idx = await loadIndex();
    sel = 0;
    render(list, search(idx, input.value), input.value);
  };
  input.addEventListener('focus', () => void loadIndex(), { once: true });
  input.addEventListener('input', run);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'Enter') {
      const a = list.querySelectorAll<HTMLAnchorElement>('[role=option] a')[sel];
      if (a) { e.preventDefault(); location.href = a.href; }
    }
  });
  root.querySelectorAll<HTMLButtonElement>('[data-chip]').forEach((b) =>
    b.addEventListener('click', () => { input.value = b.dataset.chip!; input.focus(); void run(); }),
  );
}

export function initFinders() {
  document.querySelectorAll<HTMLElement>('[data-finder]:not([data-wired])').forEach((el) => {
    el.dataset.wired = '1';
    wire(el);
  });
  const dialog = document.querySelector<HTMLDialogElement>('dialog[data-finder-dialog]');
  if (!dialog || dialog.dataset.wired) return;
  dialog.dataset.wired = '1';
  const open = () => {
    dialog.showModal();
    dialog.querySelector('input')?.focus();
  };
  document.querySelectorAll('[data-open-finder]').forEach((b) => b.addEventListener('click', open));
  dialog.addEventListener('click', (e) => { if (e.target === dialog) dialog.close(); });
  window.addEventListener('keydown', (e) => {
    const typing = (e.target as HTMLElement)?.closest?.('input, textarea, [contenteditable]');
    if ((e.key === 'k' && (e.metaKey || e.ctrlKey)) || (e.key === '/' && !typing)) {
      e.preventDefault();
      dialog.open ? dialog.close() : open();
    }
  });
}
