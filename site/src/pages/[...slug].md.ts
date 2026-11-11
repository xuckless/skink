// Every docs page as plain Markdown at the same path plus .md
// (/tools/git/ -> /tools/git.md), for LLMs and readers who want the source.
import type { APIRoute, GetStaticPaths } from 'astro';
import { getCollection } from 'astro:content';
import { docMarkdown, manualMarkdown, manualTopics } from '../lib/markdown';

export const getStaticPaths: GetStaticPaths = async () => {
  const docs = await getCollection('docs');
  return [
    ...docs.map((d) => ({ params: { slug: d.id }, props: { md: docMarkdown(d) } })),
    ...manualTopics.map((t) => ({ params: { slug: `reference/manual/${t.slug}` }, props: { md: manualMarkdown(t) } })),
  ];
};

export const GET: APIRoute = ({ props }) =>
  new Response(props.md as string, { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
