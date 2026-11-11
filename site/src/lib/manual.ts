// The :help pages (crates/skink-ui/src/help/*.md) on the site: Markdown to
// HTML, links between topics pointed at their pages, keys drawn as <kbd>.
import { markdownToHtml } from 'satteri';
import { ref, manualUrl } from './reference';

const SITE = 'https://skink.cc';

// A help link is a topic id ("packages", "keys#tabs"); the generated topics
// have their own reference pages on the site.
const GENERATED: Record<string, string> = {
  index: '/reference/manual/',
  keys: '/reference/shortcuts/',
  actions: '/reference/shortcuts/',
  commands: '/reference/commands/',
};
export function topicUrl(id: string): string | undefined {
  if (GENERATED[id]) return GENERATED[id];
  const t = ref.manual.find((m) => m.id === id);
  return t && manualUrl(t.heading);
}
const isTopicLink = (href: string) => /^[a-z][a-z0-9-]*(#[\w-]+)?$/.test(href);
function resolve(href: string, base = ''): string {
  if (!isTopicLink(href)) return href;
  const [id, hash] = href.split('#');
  const url = topicUrl(id);
  return url ? base + url + (hash ? '#' + hash : '') : href;
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

/** Keys in running text (not in code) become <kbd>; topic links point at pages. */
function decorate(html: string): string {
  let inCode = 0;
  return html
    .split(/(<[^>]+>)/)
    .map((seg) => {
      if (seg.startsWith('<')) {
        if (/^<(code|pre)\b/.test(seg)) inCode++;
        else if (/^<\/(code|pre)>/.test(seg)) inCode--;
        return seg.replace(/^<a href="([^"]*)"/, (_, h) => `<a href="${resolve(h)}"`);
      }
      return inCode ? seg : seg.replace(KEY, (k) => kbd(k));
    })
    .join('');
}

export async function manualHtml(body: string): Promise<string> {
  const { html } = await markdownToHtml(body, { features: { gfm: true } });
  return decorate(html);
}

/** The topic as Markdown for the .md copies and llms-full.txt, links absolute. */
export function manualMd(body: string): string {
  return body.replace(/\]\(([^)\s]+)\)/g, (all, href) => (isTopicLink(href) ? `](${resolve(href, SITE)})` : all));
}
