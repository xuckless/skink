import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://xuckless.github.io',
  base: '/skink',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Skink',
      description:
        'Skink is a fast, modeless terminal code editor: Neovim power with mouse and Micro keys, Zed-style features inside the terminal, built for SSH and AI agents.',
      logo: { light: './src/assets/logo-light.svg', dark: './src/assets/logo-dark.svg', replacesTitle: true },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/xuckless/skink' }],
      editLink: { baseUrl: 'https://github.com/xuckless/skink/edit/main/site/' },
      lastUpdated: true,
      customCss: ['./src/styles/theme.css'],
      components: { Head: './src/components/Head.astro', Search: './src/components/Search.astro' },
      pagefind: false,
      head: [
        { tag: 'meta', attrs: { property: 'og:image', content: 'https://xuckless.github.io/skink/og.png' } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
      ],
      sidebar: [
        { label: 'Start here', items: [{ autogenerate: { directory: 'start' } }] },
        { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
        { label: 'Compare', collapsed: true, items: [{ autogenerate: { directory: 'compare' } }] },
        { label: 'Articles', collapsed: true, items: [{ autogenerate: { directory: 'articles' } }] },
      ],
    }),
  ],
});
