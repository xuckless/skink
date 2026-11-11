// Turns help.txt's terminal layout (aligned columns, hanging indents) into
// rich blocks: paragraphs, term/description lists and code.
export type Block =
  | { type: 'p'; text: string }
  | { type: 'dl'; items: { term: string; desc: string[]; code: string[] }[] }
  | { type: 'code'; lines: string[] };

const SPLIT = /^(\S(?:.*?\S)?)\s{2,}(\S.*)$/;

export function parseManual(body: string): Block[] {
  const blocks: Block[] = [];
  type Item = { term: string; desc: string[]; code: string[]; indent: number; col: number };
  let dl: { type: 'dl'; items: Item[] } | null = null;
  let para: { type: 'p'; text: string; indent: number } | null = null;
  let code: { type: 'code'; lines: string[] } | null = null;
  const close = () => {
    dl = null;
    para = null;
    code = null;
  };
  for (const raw of body.split('\n')) {
    if (!raw.trim()) {
      close();
      continue;
    }
    const indent = raw.length - raw.trimStart().length;
    const text = raw.trim();
    const item = dl?.items.at(-1);
    if (item && indent > item.indent && indent >= item.col - 1) {
      // Deeper lines are a code example only after a line ending in ':' (or
      // inside one); otherwise they are a wrapped description.
      const lead = item.desc.at(-1) ?? '';
      if (indent > item.col + 1 && (item.code.length > 0 || /:\s*$/.test(lead))) item.code.push(raw.slice(item.col + 2));
      else item.desc.push(text);
      continue;
    }
    const m = text.match(SPLIT);
    if (m && m[1].length <= 34 && !/[.;]$/.test(m[1])) {
      if (!dl) {
        dl = { type: 'dl', items: [] };
        blocks.push(dl);
        para = null;
        code = null;
      }
      dl.items.push({ term: m[1], desc: [m[2]], code: [], indent, col: indent + raw.trimStart().indexOf(m[2]) });
      continue;
    }
    if (para && indent > para.indent + 1 && !code && !/:\s*$/.test(para.text)) {
      para.text += ' ' + text;
      continue;
    }
    if (para && indent > para.indent + 1) {
      if (!code) {
        code = { type: 'code', lines: [] };
        blocks.push(code);
      }
      code.lines.push(raw.slice(para.indent + 2));
      continue;
    }
    if (para && !code && indent === para.indent) {
      para.text += ' ' + text;
      continue;
    }
    dl = null;
    code = null;
    para = { type: 'p', text, indent };
    blocks.push(para);
  }
  return blocks.map((b) =>
    b.type === 'dl'
      ? { type: 'dl', items: b.items.map(({ term, desc, code }) => ({ term, desc, code })) }
      : b.type === 'p'
        ? { type: 'p', text: b.text }
        : b,
  );
}

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);

const KEY =
  /(?<![\w-])((?:(?:Ctrl|Alt|Shift)-)+(?:F\d{1,2}|PageUp|PageDown|Up|Down|Left|Right|Home|End|Enter|Space|Tab|Delete|Backspace|Esc|[A-Za-z0-9](?![\w])|[\\/=,.\][;`-])|F\d{1,2}(?![\w-])|Esc(?![\w]))/g;

export function kbd(key: string) {
  const parts: string[] = [];
  let rest = key;
  for (;;) {
    const m = rest.match(/^(Ctrl|Alt|Shift)-(.+)$/);
    if (!m) break;
    parts.push(m[1]);
    rest = m[2];
  }
  parts.push(rest.length === 1 ? rest.toUpperCase() : rest);
  return parts.map((p) => `<kbd>${esc(p)}</kbd>`).join('<span class="plus">+</span>');
}

/** Inline markup: `code`, keys as <kbd>, :commands as code. */
export function inline(text: string, columns = false) {
  return text
    .replace(/(\{"[^}]*\})/g, '`$1`')
    .split(/(`[^`]+`)/)
    .map((seg) => {
      if (seg.startsWith('`') && seg.endsWith('`') && seg.length > 1) return `<code>${esc(seg.slice(1, -1))}</code>`;
      return esc(seg)
        .replace(/\s{3,}/g, columns ? ' · ' : ' ')
        .replace(KEY, (k) => kbd(k))
        .replace(/(^|[\s(])(:[a-z][\w-]*!?)/g, '$1<code class="cmd">$2</code>');
    })
    .join('');
}
