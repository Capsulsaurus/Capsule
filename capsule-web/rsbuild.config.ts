import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';
import { TanStackRouterRspack } from '@tanstack/router-plugin/rspack';

const isDev = process.env.NODE_ENV === 'development';

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
            plugins: [TanStackRouterRspack()],
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
