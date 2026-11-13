// When a page last really changed: the last commit that touched its source file.
// Used for the sitemap <lastmod> and the structured data's dateModified, so both
// stay put across rebuilds that change nothing (the site rebuilds daily).
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import ref from '../data/reference.json' with { type: 'json' };

// The build runs in site/ (import.meta.url points into the bundle during a build).
const root = process.cwd() + '/';
const slug = (h) => h.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
const helpFile = new Map(ref.manual.map((m) => [slug(m.heading), `../crates/skink-ui/src/help/${m.id}.md`]));

export function sourceOf(pathname) {
  const id = pathname.replace(/^\/|\/$/g, '');
  if (!id) return 'src/pages/index.astro';
  if (id === 'reference/manual') return 'src/content/docs/reference/manual.mdx';
  const manual = id.match(/^reference\/manual\/([^/]+)$/);
  if (manual) return helpFile.get(manual[1]);
  if (id === 'packages') return 'src/pages/packages/index.astro';
  for (const f of [`src/content/docs/${id}.mdx`, `src/content/docs/${id}/index.mdx`]) if (existsSync(root + f)) return f;
  return undefined;
}

const cache = new Map();
export function lastCommit(path) {
  if (!path) return undefined;
  if (!cache.has(path)) {
    let d;
    try {
      d = execFileSync('git', ['log', '-1', '--format=%cI', '--', path], { cwd: root, encoding: 'utf8' }).trim() || undefined;
    } catch {
      d = undefined;
    }
    cache.set(path, d);
  }
  return cache.get(path);
}

export const lastModified = (pathname) => lastCommit(sourceOf(pathname));
