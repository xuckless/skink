import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';
import { lastModified } from './src/lib/lastmod.mjs';
import ref from './src/data/reference.json' with { type: 'json' };

export const SITE = 'https://skink.cc';

const slug = (h) => h.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

export default defineConfig({
  site: SITE,
  trailingSlash: 'always',
  build: { format: 'directory' },
  integrations: [
    sitemap({
      filter: (page) => !page.endsWith('/404/'),
      serialize(item) {
        const lastmod = lastModified(new URL(item.url).pathname);
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
      routeMiddleware: './src/routeData.ts',
      pagefind: false,
      head: [
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
        { tag: 'meta', attrs: { name: 'twitter:site', content: '@xuckless' } },
        { tag: 'link', attrs: { rel: 'icon', href: '/favicon.ico', sizes: '32x32' } },
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
        {
          label: 'Customize',
          items: [{ autogenerate: { directory: 'customize' } }, { label: 'Package list', link: '/packages/' }],
        },
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
