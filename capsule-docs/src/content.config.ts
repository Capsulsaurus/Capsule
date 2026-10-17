import { defineCollection, z } from 'astro:content';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';
import { docsVersionsLoader } from 'starlight-versions/loader';

export const collections = {
    docs: defineCollection({
        loader: docsLoader(),
        schema: docsSchema({
            extend: z.object({
                // Review state: `draft` until a human re-review passes the doc,
                // then flipped to `stable`. See Core Principles — Doc Structure.
                // Required, not optional: the field only means anything if a new
                // page cannot land unmarked, and `astro build` — already the
                // `build-docs` gate — is what enforces it.
                status: z.enum(['draft', 'stable']),
            }),
        }),
    }),
    versions: defineCollection({ loader: docsVersionsLoader() }),
};
