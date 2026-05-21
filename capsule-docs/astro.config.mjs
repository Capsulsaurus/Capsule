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
                    autogenerate: { directory: 'design' },
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
