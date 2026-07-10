import { defineCollection, z } from 'astro:content';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';
import { docsVersionsLoader } from 'starlight-versions/loader';

export const collections = {
    docs: defineCollection({
        loader: docsLoader(),
        schema: docsSchema({
            extend: z.object({
                // Design-doc review state: `draft` until a human re-review passes the
                // doc, then flipped to `stable`. See Core Principles — Doc Structure.
                status: z.enum(['draft', 'stable']).optional(),
            }),
        }),
    }),
    versions: defineCollection({ loader: docsVersionsLoader() }),
};
