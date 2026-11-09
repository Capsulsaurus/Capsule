import { fileURLToPath } from 'node:url';
import { unified } from '@astrojs/markdown-remark';
import starlight from '@astrojs/starlight';
import tailwindcss from '@tailwindcss/vite';
// @ts-check
import { defineConfig } from 'astro/config';
import starlightBlog from 'starlight-blog';
import starlightLinksValidator from 'starlight-links-validator';
import { referenceSidebar } from './scripts/reference-groups.mjs';
import { hasPublishedPost } from './src/lib/blog-posts.mjs';
import rehypeNoTranslate from './src/lib/rehype-notranslate.mjs';

// The blog's header link and RSS feed appear only once a post is published,
// so no reader lands on an empty index. SiteFooter.astro applies the same rule.
const blogIsLive = hasPublishedPost(
    fileURLToPath(new URL('./src/content/docs/blog/', import.meta.url)),
);

// https://astro.build/config
export default defineConfig({
    site: 'https://capsule.justinchung.net', // TODO: Get domain later
    // Opt technical terms out of browser/machine auto-translation. See the plugin.
    // Astro 7 deprecates the top-level `markdown.rehypePlugins` key; plugins are
    // passed to the unified processor directly.
    markdown: {
        processor: unified({ rehypePlugins: [rehypeNoTranslate] }),
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
                                { slug: 'design/module-map' },
                                { slug: 'design/api-surfaces' },
                                { slug: 'design/developer-docs' },
                                { slug: 'design/networking' },
                                { slug: 'design/dependencies' },
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
                                { slug: 'design/cryptography/failure-modes' },
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
                starlightBlog({
                    title: 'Blog',
                    navigation: blogIsLive ? 'header-end' : 'none',
                    rss: blogIsLive,
                    authors: {
                        justin: {
                            name: 'Justin Chung',
                            url: 'https://github.com/justin13888',
                        },
                    },
                }),
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
    },
});
