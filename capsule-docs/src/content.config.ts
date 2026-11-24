import { defineCollection, z } from 'astro:content';
import { docsLoader, i18nLoader } from '@astrojs/starlight/loaders';
import { docsSchema, i18nSchema } from '@astrojs/starlight/schema';
import { blogSchema } from 'starlight-blog/schema';
import {
    COMMIT_PATTERN,
    DATE_PATTERN,
    DIGEST_PATTERN,
} from './lib/page-audit.mjs';

export const collections = {
    docs: defineCollection({
        loader: docsLoader(),
        schema: docsSchema({
            extend: (context) =>
                blogSchema(context).extend({
                    // Audit pin: `none`, or the commit, date and content digest
                    // of the page's last audit. The status shown (audited,
                    // outdated, unaudited) is computed from it by
                    // src/lib/page-audit.mjs, never written down; stamp it with
                    // `bun run audit-page <file>`. See Core Principles — Audit
                    // Status. Optional here only because plugin routes (the
                    // blog index, tag and author pages) render through
                    // <StarlightPage> with no frontmatter of their own. Every
                    // authored page must still set it:
                    // scripts/page-audit.test.mjs, part of `test-docs`, fails on
                    // any page that does not, bar a splash page, which has no
                    // sidebar to badge. A generated page carries none: it
                    // tracks its artifact, not a review, and PageTitle.astro
                    // refuses one that sets both.
                    audit: z
                        .union([
                            z.literal('none'),
                            z
                                .object({
                                    commit: z.string().regex(COMMIT_PATTERN),
                                    // YAML reads an unquoted date as a Date.
                                    date: z.preprocess(
                                        (value) =>
                                            value instanceof Date
                                                ? value
                                                      .toISOString()
                                                      .slice(0, 10)
                                                : value,
                                        z.string().regex(DATE_PATTERN),
                                    ),
                                    digest: z.string().regex(DIGEST_PATTERN),
                                })
                                .strict(),
                        ])
                        .optional(),
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
    // zh-Hans.json and zh-Hant.json are Starlight's own zh-CN and zh-TW
    // strings: Starlight finds its built-in dictionary by language and
    // region, so a script-tagged language (src/lib/locales.mjs) would
    // otherwise render its interface in English. src/lib/locales.test.mjs
    // keeps them in step with Starlight.
    i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() }),
};
