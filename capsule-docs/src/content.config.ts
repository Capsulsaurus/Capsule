import { defineCollection, z } from 'astro:content';
import { docsLoader, i18nLoader } from '@astrojs/starlight/loaders';
import { docsSchema, i18nSchema } from '@astrojs/starlight/schema';
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
                    // on any page that does not. A generated page carries
                    // none: it tracks its artifact, not a review, and
                    // PageTitle.astro refuses one that sets both.
                    status: z.enum(['draft', 'stable']).optional(),
                    // Set only by scripts/gen-reference.mjs: which surface a
                    // generated page documents, the artifact it came from and,
                    // when served, where to download it. PageTitle.astro
                    // renders it as the generated-page banner.
                    generated: z
                        .object({
                            surface: z.string(),
                            source: z.string(),
                            download: z.string().optional(),
                            // The artifact's source changed on this branch after it
                            // did (scripts/lib/staleness.mjs): a dev-only caution.
                            stale: z.boolean().optional(),
                        })
                        .optional(),
                }),
        }),
    }),
    // Overrides of UI strings, keyed as in Starlight and its plugins. The blog
    // plugin's metadata strings carry their own " - " separators; the post
    // header supplies its own (src/styles/global.css), so these drop them.
    i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() }),
    versions: defineCollection({ loader: docsVersionsLoader() }),
};
