import { GenerateSW, InjectManifest } from '@aaroon/workbox-rspack-plugin';
import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';
import { TanStackRouterRspack } from '@tanstack/router-plugin/rspack';

const isDev = process.env.NODE_ENV === 'development';

const swPlugin = new GenerateSW({
    clientsClaim: true,
    skipWaiting: true,
    runtimeCaching: [
        {
            // TODO: Configure this vv
            urlPattern: /^https:\/\/your-api\.com\/.*/, // Cache API calls
            handler: 'NetworkFirst',
            options: {
                cacheName: 'api-cache',
                expiration: {
                    maxEntries: 50,
                    maxAgeSeconds: 60 * 60 * 24, // 1 day
                },
            },
        },
        {
            urlPattern: /\.(?:png|jpg|jpeg|svg|gif)$/, // Cache images
            handler: 'CacheFirst',
            options: {
                cacheName: 'image-cache',
                expiration: {
                    maxEntries: 100,
                    maxAgeSeconds: 60 * 60 * 24 * 30, // 30 days
                },
            },
        },
        {
            urlPattern: ({ request }) => request.destination === 'document', // Cache HTML pages
            handler: 'NetworkFirst',
            options: {
                cacheName: 'html-cache',
            },
        },
    ],
});

export default defineConfig({
    plugins: [pluginReact()],
    // dev: {
    //   lazyCompilation: true, // Breaks UI
    // },
    html: {
        meta: {
            'theme-color': '#000000',
        },
        title: 'Capsule',
        // favicon: './src/assets/favicon.ico', // TODO
        appIcon: {
            name: 'Capsule',
            filename: 'manifest.json',
            icons: [
                { src: './src/assets/icon-192.png', size: 192 },
                { src: './src/assets/icon-512.png', size: 512 },
            ],
        },
    },
    server: {
        port: 5173,
    },
    tools: {
        rspack: {
            plugins: [TanStackRouterRspack(), swPlugin],
            experiments: {
                // Might break TailwindCSS V4/CSS Variables
                incremental: isDev,
            },
        },
    },
    performance: {
        buildCache: isDev,
        removeConsole: !isDev,
    },
});
