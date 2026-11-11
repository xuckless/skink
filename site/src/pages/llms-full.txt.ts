// Every docs page and manual topic as Markdown, in reading order.
import type { APIRoute } from 'astro';
import { getCollection } from 'astro:content';
import { SECTIONS, MORE, inSection } from '../lib/sections';
import { docMarkdown, manualMarkdown, manualTopics } from '../lib/markdown';

export const GET: APIRoute = async () => {
  const docs = await getCollection('docs');
  const parts: string[] = [];
  for (const s of [...SECTIONS, ...MORE]) {
    for (const d of inSection(docs, s.dir)) parts.push(docMarkdown(d));
    if (s.dir === 'reference') for (const t of manualTopics) parts.push(manualMarkdown(t));
  }
  return new Response(parts.join('\n---\n\n'), { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
