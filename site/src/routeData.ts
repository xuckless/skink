import { defineRouteMiddleware } from '@astrojs/starlight/route-data';

const setTitle = (head: { tag: string; content?: string }[], title: string) => {
  const t = head.find((h) => h.tag === 'title');
  if (t) t.content = title;
  else head.push({ tag: 'title', content: title } as never);
};

export const onRequest = defineRouteMiddleware((context) => {
  const route = context.locals.starlightRoute;
  const path = context.url.pathname;
  // Manual topics are named after one word ("Git", "AI"); give the <title> enough context to stand alone.
  if (/^\/reference\/manual\/[^/]+\/$/.test(path)) setTitle(route.head, `${route.entry.data.title} in the Skink manual`);
  if (path === '/packages/') setTitle(route.head, 'Skink packages: grammars, language servers and themes');
  if (route.entry.id !== '404') return;
  route.head = route.head.filter((h) => !(h.tag === 'link' && h.attrs?.rel === 'canonical'));
  route.head.push({ tag: 'meta', attrs: { name: 'robots', content: 'noindex' } });
});
