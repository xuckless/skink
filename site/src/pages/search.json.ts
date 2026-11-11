// The finder's index: every docs page plus the generated shortcuts,
// commands and manual sections. Built once, fetched on first search.
import { getCollection } from 'astro:content';
import ref from '../data/reference.json';
import { manualSlug } from '../lib/reference';

const BASE = '/';

const strip = (md: string) =>
  md
    .replace(/^import .*$/gm, '')
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/<[^>]+>/g, ' ')
    .replace(/[#*_`>|[\]()-]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 700);

export async function GET() {
  const docs = await getCollection('docs');
  const items: Record<string, unknown>[] = [];
  for (const d of docs) {
    items.push({
      t: d.data.title,
      k: d.data.kind ?? 'guide',
      u: `${BASE}${d.id}/`,
      a: d.data.answer ?? d.data.description ?? '',
      w: d.data.keywords.join(' '),
      x: strip(d.body ?? ''),
    });
  }
  for (const key of ref.keys) {
    items.push({
      t: key.about,
      k: 'key',
      u: `${BASE}reference/shortcuts/#${key.action.split(/[|,]/)[0].replace(/[^A-Za-z]/g, '')}`,
      kb: key.key,
      a: key.typed ? `Or press Ctrl+E and type ${key.typed}` : undefined,
      w: `${key.action.replace(/([a-z])([A-Z])/g, '$1 $2')} ${key.key.replace(/\+/g, ' ')} ${key.typed ?? ''}`,
    });
  }
  for (const c of ref.commands) {
    items.push({
      t: `:${c.name}${c.args ? ' ' + c.args : ''}`,
      k: 'command',
      u: `${BASE}reference/commands/#${c.name}`,
      a: c.about,
      w: [c.name, ...c.aliases].join(' '),
    });
  }
  for (const m of ref.manual) {
    items.push({
      t: m.heading,
      k: 'manual',
      u: `${BASE}reference/manual/${manualSlug(m.heading)}/`,
      a: m.body.split('\n').find((l) => l.trim())?.trim() ?? '',
      w: m.heading,
      x: m.body.replace(/\s+/g, ' ').slice(0, 1500),
    });
  }
  return new Response(JSON.stringify(items), { headers: { 'Content-Type': 'application/json' } });
}
