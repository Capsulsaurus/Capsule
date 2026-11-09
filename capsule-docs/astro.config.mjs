import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { unified } from '@astrojs/markdown-remark';
import starlight from '@astrojs/starlight';
import tailwindcss from '@tailwindcss/vite';
// @ts-check
import { defineConfig } from 'astro/config';
import starlightBlog from 'starlight-blog';
import starlightLinksValidator from 'starlight-links-validator';
import { referenceSidebar } from './scripts/reference-groups.mjs';
import { BLOG_AUTHORS, hasPublishedPost } from './src/lib/blog-posts.mjs';
import { resolveBuildCommit } from './src/lib/build-commit.mjs';
import rehypeNoTranslate from './src/lib/rehype-notranslate.mjs';
import remarkLibraryLinks from './src/lib/remark-library-links.mjs';
import remarkLinkTitles from './src/lib/remark-link-titles.mjs';
import remarkMermaid from './src/lib/remark-mermaid.mjs';

// A production build includes the blog (routes, header link, RSS, sitemap,
// footer link) only once a post is published, so no reader lands on an empty
// index. `astro dev` always includes it, so a draft previews in the blog
// layout. Read once at startup: restart the dev server after publishing.
const blogIsLive =
    !process.argv.includes('build') ||
    hasPublishedPost(
        fileURLToPath(new URL('./src/content/docs/blog/', import.meta.url)),
    );

// The commit this build was made from, shown in the footer. `null` (and no
// footer segment) when neither the host nor git names one.
const buildCommit = resolveBuildCommit(process.env, () => {
    try {
        return execFileSync('git', ['rev-parse', 'HEAD'], {
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'ignore'],
        });
    } catch {
        return null;
    }
});

// https://astro.build/config
export default defineConfig({
    site: 'https://capsule.justinchung.net', // TODO: Get domain later
    // Retired pages; their content moved to the targets or into code docs.
    redirects: {
        '/design/module-map/': '/design/principles/',
        '/design/dependencies/': '/design/licensing/',
        '/design/cryptography/failure-modes/': '/design/backup-recovery/#recovery-paths',
        '/reference/crates/': '/reference/sdk/rust/',
    },
    // Astro 7 deprecates the top-level `markdown.*Plugins` keys; plugins are
    // passed to the unified processor directly. See each plugin's header.
    markdown: {
        processor: unified({
            // An empty-text site link renders its target's title or heading.
            remarkPlugins: [
                // ```mermaid fences compile to themed inline SVG.
                remarkMermaid,
                [
                    remarkLinkTitles,
                    {
                        contentDir: fileURLToPath(
                            new URL('./src/content/docs/', import.meta.url),
                        ),
                    },
                ],
                // The first mention of each library links its source repo.
                remarkLibraryLinks,
            ],
            // Opts technical terms out of browser/machine auto-translation.
            rehypePlugins: [rehypeNoTranslate],
        }),
    },
    integrations: [
        starlight({
            title: 'Capsule',
            description:
                'An open-source, end-to-end encrypted photo library: organization that runs on your devices, a server that holds no keys, and sharing designed to span servers.',
            favicon: '/favicon.svg',
            social: [
                {
                    icon: 'github',
                    label: 'GitHub',
                    href: 'https://github.com/Capsulsaurus/Capsule',
                },
            ],
            editLink: {
                baseUrl:
                    'https://github.com/Capsulsaurus/Capsule/tree/master/capsule-docs',
            },
            components: {
                // Renders the `status` frontmatter (draft/stable) as a title badge.
                PageTitle: './src/components/PageTitle.astro',
                // Appends the site footer (links, licence) to every page.
                Footer: './src/components/Footer.astro',
                // The header: wordmark, section links, GitHub button and the
                // three-way theme toggle. See development/visual-language.md.
                Header: './src/components/Header.astro',
                SiteTitle: './src/components/SiteTitle.astro',
                SocialIcons: './src/components/SocialIcons.astro',
                ThemeSelect: './src/components/ThemeToggle.astro',
                // Carries the section links on narrow screens.
                MobileMenuFooter: './src/components/MobileMenuFooter.astro',
            },
            // Badges every non-stable page in the sidebar. See src/route-data.ts.
            routeMiddleware: './src/route-data.ts',
            sidebar: [
                {
                    label: 'Guides',
                    items: [
                        // Each item here is one entry in the navigation menu.
                        {
                            label: 'Getting Started',
                            slug: 'guides/getting-started',
                        },
                        { slug: 'guides/user-guide' },
                        { slug: 'guides/self-hosting' },
                        { slug: 'guides/google-photos-migration' },
                    ],
                },
                {
                    label: 'Features',
                    items: [{ autogenerate: { directory: 'features' } }],
                },
                {
                    label: 'Design',
                    items: [
                        {
                            label: 'Foundations',
                            items: [
                                { slug: 'design' },
                                { slug: 'design/principles' },
                                { slug: 'design/api-surfaces' },
                                { slug: 'design/developer-docs' },
                                { slug: 'design/networking' },
                                { slug: 'design/licensing' },
                            ],
                        },
                        {
                            label: 'Cryptography',
                            items: [
                                { slug: 'design/cryptography' },
                                { slug: 'design/cryptography/primitives' },
                                { slug: 'design/cryptography/keys' },
                                { slug: 'design/cryptography/encryption' },
                                { slug: 'design/cryptography/mls' },
                                { slug: 'design/cryptography/provenance' },
                            ],
                        },
                        {
                            label: 'Identity & Access',
                            items: [
                                { slug: 'design/authentication' },
                                { slug: 'design/authorization' },
                                { slug: 'design/device-enrollment' },
                                { slug: 'design/activity-log' },
                                { slug: 'design/mls-resilience' },
                            ],
                        },
                        {
                            label: 'Storage',
                            items: [
                                { slug: 'design/filesystem' },
                                { slug: 'design/filesystem/server' },
                                { slug: 'design/filesystem/client' },
                                { slug: 'design/filesystem/maintenance' },
                                { slug: 'design/metadata' },
                                { slug: 'design/thumbnails' },
                                { slug: 'design/quota' },
                            ],
                        },
                        {
                            label: 'Import, Transfer & Sync',
                            items: [
                                { slug: 'design/import' },
                                { slug: 'design/import/pipeline' },
                                { slug: 'design/import/camera-import' },
                                { slug: 'design/transfer' },
                                { slug: 'design/transfer/upload' },
                                { slug: 'design/transfer/download' },
                                { slug: 'design/transfer/durability' },
                                { slug: 'design/sync' },
                                { slug: 'design/backup-recovery' },
                                { slug: 'design/versioning' },
                            ],
                        },
                        {
                            label: 'Sharing & Federation',
                            items: [
                                { slug: 'design/federation' },
                                { slug: 'design/peering' },
                                { slug: 'design/share-links' },
                                { slug: 'design/web-upload' },
                                { slug: 'design/moderation' },
                            ],
                        },
                        {
                            label: 'Organization & Clients',
                            items: [
                                { slug: 'design/organization' },
                                { slug: 'design/clients' },
                                { slug: 'design/local-gallery' },
                                { slug: 'design/notifications' },
                                { slug: 'design/i18n' },
                                { slug: 'design/ai' },
                            ],
                        },
                        {
                            label: 'Threat Model',
                            items: [
                                { slug: 'design/threat-model' },
                                { slug: 'design/threat-model/scenarios' },
                                { slug: 'design/threat-model/schema-rules' },
                                { slug: 'design/threat-model/validation' },
                            ],
                        },
                    ],
                },
                {
                    label: 'Development',
                    items: [{ autogenerate: { directory: 'development' } }],
                },
                {
                    // Hand-curated, not autogenerated: generated reference pages must not
                    // determine navigation order. See design/developer-docs.md.
                    //
                    // Curated in `scripts/reference-groups.mjs` rather than inline, because
                    // the generator reads the same list to decide which pages exist. One
                    // edit moves a page and its navigation entry together, and a group
                    // with navigation but no page is not expressible.
                    label: 'Reference',
                    items: referenceSidebar(),
                },
            ],
            customCss: ['./src/styles/global.css'],
            // TODO: Add internationalization down the line: https://starlight.astro.build/guides/i18n/
            plugins: [
                ...(blogIsLive
                    ? [
                          starlightBlog({
                              title: 'Blog',
                              // The header's own section links carry Blog
                              // (src/lib/site-nav.mjs), so the plugin adds none.
                              navigation: 'none',
                              metrics: { readingTime: true },
                              postCount: 10,
                              recentPostCount: 5,
                              authors: BLOG_AUTHORS,
                          }),
                      ]
                    : []),
                // Feature cards on the front page link into the design docs.
                starlightLinksValidator({ components: [['Feature', 'href']] }),
                // starlightVersions({
                // 	// current: {
                // 	// 	label: 'master',
                // 	// },
                // 	versions: [
                // 		{ slug: 'Latest' }
                // 	],
                // }), // TODO: Add versions later
            ],
        }),
    ],
    vite: {
        plugins: [tailwindcss()],
        // Read by SiteFooter.astro and HeaderNav.astro, so their Blog links
        // follow the same rule, and the footer names the built commit.
        define: {
            __BLOG_IS_LIVE__: JSON.stringify(blogIsLive),
            __BUILD_COMMIT__: JSON.stringify(buildCommit),
        },
    },
});
