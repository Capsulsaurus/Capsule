import starlight from '@astrojs/starlight';
import tailwindcss from '@tailwindcss/vite';
// @ts-check
import { defineConfig } from 'astro/config';
import starlightLinksValidator from 'starlight-links-validator';
import starlightVersions from 'starlight-versions';

// https://astro.build/config
export default defineConfig({
    site: 'https://capsule.justinchung.net', // TODO: Get domain later
    integrations: [
        starlight({
            title: 'Capsule',
            description: 'Photo sharing for all!',
            social: [
                {
                    icon: 'github',
                    label: 'GitHub',
                    href: 'https://github.com/justin13888/Capsule',
                },
            ],
            editLink: {
                baseUrl:
                    'https://github.com/justin13888/Capsule/tree/master/capsule-docs',
            },
            sidebar: [
                {
                    label: 'Guides',
                    items: [
                        // Each item here is one entry in the navigation menu.
                        {
                            label: 'Getting Started',
                            slug: 'guides/getting-started',
                        },
                        { slug: 'guides/self-hosting' },
                    ],
                },
                {
                    label: 'Features',
                    autogenerate: { directory: 'features' },
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
                            label: 'Import & Sync',
                            items: [
                                { slug: 'design/import' },
                                { slug: 'design/import/pipeline' },
                                { slug: 'design/import/upload-protocol' },
                                { slug: 'design/import/download-sync' },
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
                                { slug: 'design/moderation' },
                            ],
                        },
                        {
                            label: 'Organization & Clients',
                            items: [
                                { slug: 'design/organization' },
                                { slug: 'design/clients' },
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
                    autogenerate: { directory: 'development' },
                },
                {
                    label: 'Reference',
                    autogenerate: { directory: 'reference' },
                },
            ],
            customCss: ['./src/styles/global.css'],
            // TODO: Add internationalization down the line: https://starlight.astro.build/guides/i18n/
            plugins: [
                starlightLinksValidator(),
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
