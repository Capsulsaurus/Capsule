import { defineCollection, z } from 'astro:content';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';
import { blogSchema } from 'starlight-blog/schema';
import { docsVersionsLoader } from 'starlight-versions/loader';

export const collections = {
    docs: defineCollection({
        loader: docsLoader(),
        schema: docsSchema({
            extend: (context) =>
                blogSchema(context).extend({
                    // Review state: `draft` until a human re-review passes the doc,
                    // then flipped to `stable`. See Core Principles — Doc Structure.
                    // Optional here only because plugin routes (the blog index, tag
                    // and author pages) render through <StarlightPage> with no
                    // frontmatter of their own. Every authored page must still set
                    // it: scripts/page-status.test.mjs, part of `test-docs`, fails
                    // on any page that does not.
                    status: z.enum(['draft', 'stable']).optional(),
                }),
        }),
    }),
    versions: defineCollection({ loader: docsVersionsLoader() }),
};
