import { defineCollection } from 'astro:content';
import { z } from 'astro/zod';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

export const collections = {
  docs: defineCollection({
    loader: docsLoader(),
    schema: docsSchema({
      extend: z.object({
        // Words people type when they have this problem ("quit", "close", "leave").
        // Feeds the finder and the keywords meta tag.
        keywords: z.array(z.string()).default([]),
        // One-sentence answer shown straight in the finder's result.
        answer: z.string().optional(),
        // Finder badge.
        kind: z.enum(['start', 'guide', 'compare', 'article', 'reference']).optional(),
      }),
    }),
  }),
};
