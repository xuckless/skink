// Plain Markdown versions of the docs, for llms.txt, llms-full.txt and the
// per-page .md files. MDX components become plain Markdown; generated pages
// (shortcuts, commands, manual) are rendered from reference.json.
import type { CollectionEntry } from 'astro:content';
import { ref, groupKeys, manualTopics, manualGroups, manualUrl } from './reference';

export const SITE = 'https://skink.cc';

const abs = (s: string) => s.replace(/\]\((\/[^)]*)\)/g, (_, p) => `](${SITE}${p})`);

/** MDX body -> Markdown. Code fences are left untouched. */
export function mdxToMarkdown(body: string): string {
  return body
    .split(/(^```[\s\S]*?^```)/m)
    .map((part, i) => {
      if (i % 2 === 1) return part;
      return part
        .replace(/^import .*$/gm, '')
        .replace(/<kbd>([^<]*)<\/kbd>/g, '$1')
        .replace(/<Aside([^>]*)>/g, (_, attrs: string) => {
          const type = attrs.match(/type="(\w+)"/)?.[1];
          const title = attrs.match(/title="([^"]*)"/)?.[1];
          const word = { caution: 'Caution', tip: 'Tip', danger: 'Warning' }[type ?? ''] ?? 'Note';
          return `**${title ?? word}:**`;
        })
        .replace(/<\/Aside>/g, '')
        .replace(/<\/?Steps>/g, '')
        .replace(/<\/?Tabs[^>]*>/g, '')
        .replace(/<TabItem\s+label="([^"]*)"[^>]*>/g, '**$1**\n')
        .replace(/<\/TabItem>/g, '')
        .replace(/<[A-Z][A-Za-z]*\s*\/>/g, '')
        .replace(/\n{3,}/g, '\n\n');
    })
    .join('')
    .trim();
}

function shortcutsMd() {
  return groupKeys()
    .map(([group, keys]) =>
      [
        `## ${group}`,
        '',
        '| Key | Does | Or type |',
        '|---|---|---|',
        ...keys.map((k) => `| ${k.key} | ${k.about} | ${k.typed ? '`' + k.typed + '`' : ''} |`),
      ].join('\n'),
    )
    .join('\n\n');
}

function commandsMd() {
  const cmds = [...ref.commands].sort((a, b) => a.name.localeCompare(b.name));
  return [
    '| Command | What it does |',
    '|---|---|',
    ...cmds.map((c) => `| \`:${c.name}${c.args ? ' ' + c.args : ''}\` | ${c.about.replace(/\|/g, '\\|')} |`),
  ].join('\n');
}

function manualIndexMd() {
  return manualGroups()
    .map((g) => [`## ${g.name}`, '', ...g.topics.map((t) => `- [${t.heading}](${SITE}${manualUrl(t.heading)})`)].join('\n'))
    .join('\n\n');
}

export function docMarkdown(d: CollectionEntry<'docs'>): string {
  let body = mdxToMarkdown(d.body ?? '');
  if (d.id === 'reference/shortcuts') body = body.replace(/\s*$/, '\n\n' + shortcutsMd());
  if (d.id === 'reference/commands') body = body.replace(/\s*$/, '\n\n' + commandsMd());
  if (d.id === 'reference/manual') body = body.replace(/\s*$/, '\n\n' + manualIndexMd());
  const head = [`# ${d.data.title}`, '', d.data.description ? `> ${d.data.description}` : '', '', `Source: ${SITE}/${d.id}/`, ''];
  return abs([...head, body].join('\n')).trim() + '\n';
}

export function manualMarkdown(t: (typeof manualTopics)[number]): string {
  return [`# ${t.heading} (Skink manual)`, '', `Source: ${SITE}${manualUrl(t.heading)}`, '', '```text', t.body.trimEnd(), '```', ''].join('\n');
}

export { manualTopics };
