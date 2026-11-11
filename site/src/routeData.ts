// The 404 page must never be indexed: no canonical, and a noindex tag.
import { defineRouteMiddleware } from '@astrojs/starlight/route-data';

export const onRequest = defineRouteMiddleware((context) => {
  const route = context.locals.starlightRoute;
  if (route.entry.id !== '404') return;
  route.head = route.head.filter((h) => !(h.tag === 'link' && h.attrs?.rel === 'canonical'));
  route.head.push({ tag: 'meta', attrs: { name: 'robots', content: 'noindex' } });
});
