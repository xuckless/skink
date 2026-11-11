// The package index the client reads (schema 1): `skink --search`,
// `skink --install NAME` and `skink --install .`. Built by
// scripts/build-index.mjs; the page-only fields (kinds, license, stars,
// updated, lookalike_of) ride along and the client ignores them.
import type { APIRoute } from 'astro';
import data from '../../data/packages.json';

export const GET: APIRoute = () => {
  const { source: _source, ...index } = data as typeof data & { source?: string };
  return new Response(JSON.stringify(index) + '\n', { headers: { 'Content-Type': 'application/json; charset=utf-8' } });
};
