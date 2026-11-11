import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import ref from './src/data/reference.json' with { type: 'json' };

export const SITE = 'https://skink.cc';

// Sitemap <lastmod> from the last commit that touched each page's source.
const lastCommit = (path) => {
  try {
    return execFileSync('git', ['log', '-1', '--format=%cI', '--', path], { encoding: 'utf8' }).trim() || undefined;
  } catch {
    return undefined;
  }
};
const sourceOf = (pathname) => {
  const id = pathname.replace(/^\/|\/$/g, '');
  if (!id) return 'src/pages/index.astro';
  if (id.startsWith('reference/manual/')) return '../crates/skink-ui/src/help.txt';
  for (const f of [`src/content/docs/${id}.mdx`, `src/content/docs/${id}/index.mdx`]) if (existsSync(f)) return f;
  return undefined;
};

const slug = (h) => h.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

export default defineConfig({
  site: SITE,
  trailingSlash: 'always',
  build: { format: 'directory' },
  integrations: [
    sitemap({
      filter: (page) => !page.endsWith('/404/'),
      serialize(item) {
        const src = sourceOf(new URL(item.url).pathname);
        const lastmod = src && lastCommit(src);
        return lastmod ? { ...item, lastmod } : item;
      },
    }),
    starlight({
      title: 'Skink',
      description:
        'Skink is a fast, modeless terminal code editor: Neovim power with mouse and Micro keys, Zed-style features inside the terminal, built for SSH and AI agents.',
      logo: { light: './src/assets/logo-light.svg', dark: './src/assets/logo-dark.svg', replacesTitle: true },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/xuckless/skink' }],
      editLink: { baseUrl: 'https://github.com/xuckless/skink/edit/main/site/' },
      lastUpdated: true,
      customCss: ['@fontsource-variable/inter', '@fontsource-variable/jetbrains-mono', './src/styles/theme.css'],
      components: {
        Head: './src/components/Head.astro',
        Search: './src/components/Search.astro',
        PageTitle: './src/components/PageTitle.astro',
        ThemeProvider: './src/components/ThemeProvider.astro',
        ThemeSelect: './src/components/ThemeSelect.astro',
      },
      pagefind: false,
      head: [
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
        { tag: 'meta', attrs: { name: 'theme-color', content: '#16161e' } },
        { tag: 'link', attrs: { rel: 'alternate', type: 'text/plain', href: '/llms.txt', title: 'llms.txt' } },
      ],
      sidebar: [
        { label: 'Overview', link: '/docs/' },
        { label: 'Get started', items: [{ autogenerate: { directory: 'start' } }] },
        { label: 'Everyday editing', items: [{ autogenerate: { directory: 'editing' } }] },
        { label: 'Code and projects', items: [{ autogenerate: { directory: 'code' } }] },
        { label: 'Terminal, Git and AI', items: [{ autogenerate: { directory: 'tools' } }] },
        { label: 'Remote', items: [{ autogenerate: { directory: 'remote' } }] },
        { label: 'Customize', items: [{ autogenerate: { directory: 'customize' } }] },
        {
          label: 'Reference',
          items: [
            { label: 'Keyboard shortcuts', link: '/reference/shortcuts/' },
            { label: 'Commands', link: '/reference/commands/' },
            {
              label: 'Manual',
              collapsed: true,
              items: [
                { label: 'All topics', link: '/reference/manual/' },
                ...ref.manual.map((m) => ({ label: m.heading, link: `/reference/manual/${slug(m.heading)}/` })),
              ],
            },
          ],
        },
        {
          label: 'More',
          collapsed: true,
          items: [
            { label: 'Compared with other editors', link: '/compare/' },
            { label: 'Articles', link: '/articles/' },
          ],
        },
      ],
    }),
  ],
});
