// llms.txt (https://llmstxt.org): what Skink is, and where each answer lives.
// Links point at the Markdown copies; llms-full.txt has everything in one file.
import type { APIRoute } from 'astro';
import { getCollection } from 'astro:content';
import ref from '../data/reference.json';
import { SECTIONS, MORE, inSection } from '../lib/sections';
import { manualGroups, manualUrl } from '../lib/reference';
import { SITE } from '../lib/markdown';

export const GET: APIRoute = async () => {
  const docs = await getCollection('docs');
  const link = (d: (typeof docs)[number]) => `- [${d.data.sidebar?.label ?? d.data.title}](${SITE}/${d.id}.md): ${d.data.description ?? ''}`;
  const out = [
    '# Skink',
    '',
    `> Skink is a free, open-source (MIT OR Apache-2.0), modeless code editor that runs in the terminal on Linux, macOS and Windows. It uses Micro / VS Code style keys (Ctrl+S save, Ctrl+Q quit, Ctrl+C copy) with full mouse support, and ships language servers, tree-sitter highlighting, git, terminal tabs, a debugger and AI agents (Agent Client Protocol) in one ~22 MB binary. More languages, language servers and themes install as packages (\`skink --install NAME\`), Lua scripts add commands, and \`skink ssh host:path\` runs it on a remote machine. Current version: ${ref.version}.`,
    '',
    'Install: `curl -fsSL https://skink.cc/install.sh | sh` (Linux, macOS) or `irm https://skink.cc/install.ps1 | iex` (Windows PowerShell). Run `skink`, `skink file`, `skink dir` or `skink file:line:col`. Quit with Ctrl+Q. Inside the editor, Ctrl+E opens the command line and `help` opens the manual.',
    '',
    `Source code: https://github.com/xuckless/skink. Every page below is also available as HTML without the .md suffix. Everything in one file: ${SITE}/llms-full.txt`,
    '',
  ];
  for (const s of SECTIONS) {
    out.push(`## ${s.name}`, '');
    for (const d of inSection(docs, s.dir)) out.push(link(d));
    if (s.dir === 'reference') {
      for (const g of manualGroups())
        for (const t of g.topics) out.push(`- [Manual: ${t.heading}](${SITE}${manualUrl(t.heading).replace(/\/$/, '.md')}): ${t.lead.slice(0, 140)}`);
    }
    out.push('');
  }
  out.push('## Optional', '');
  for (const s of MORE) for (const d of inSection(docs, s.dir)) out.push(link(d));
  return new Response(out.join('\n') + '\n', { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
