/**
 * Astro integration: never serve a page rendered by an older Markdown
 * pipeline.
 *
 * Astro keeps every rendered page in its content store (.astro/data-store.json
 * in dev, node_modules/.astro/ for a build) and re-renders a page only when
 * its source changes, or when the Astro config, the content config or the
 * Astro version does. It decides the config changed by `JSON.stringify`-ing
 * it, which drops functions, so the remark and rehype plugins never count:
 * edit one, or a module it imports, and every page keeps its old render for
 * as long as the store lives.
 *
 * So this integration hashes the pipeline's source (each plugin entry, every
 * local module it imports, transitively, plus any extra file such as the
 * lockfile that pins its packages) and stamps the hash onto
 * `markdown.processor.options`, which Astro does stringify. A changed hash is
 * a changed config: Astro logs "Astro config changed" and clears the store.
 * Every hashed file is also a watch file, so in dev an edit restarts the
 * server, which re-reads the config and the plugins with it.
 */

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The `markdown.processor.options` key that carries the digest. */
export const CACHE_KEY_OPTION = 'pipelineDigest';

/** A relative specifier in a static import or re-export, or a dynamic import. */
const IMPORT =
    /(?:\bimport\s*(?:[\w*{}\s,$]+\s*from\s*)?|\bexport\s*[\w*{}\s,$]*\s*from\s*|\bimport\s*\(\s*)['"](\.{1,2}\/[^'"]+)['"]/g;

/**
 * The relative module specifiers `source` imports, in order. Packages and
 * built-ins are left out: the lockfile pins those.
 *
 * @param {string} source
 * @returns {string[]}
 */
export function localImports(source) {
    return [...source.matchAll(IMPORT)].map((match) => match[1]);
}

/** @param {string | URL} file */
const toPath = (file) => (file instanceof URL ? fileURLToPath(file) : file);

/**
 * Every file the entries load from this repository: the entries and their
 * local imports, transitively, each once, sorted. A missing import throws, so
 * the digest never quietly covers less than the pipeline.
 *
 * @param {(string | URL)[]} entries
 * @returns {string[]}
 */
export function pipelineFiles(entries) {
    /** @type {Set<string>} */
    const seen = new Set();
    const pending = entries.map(toPath);
    while (pending.length > 0) {
        const file = /** @type {string} */ (pending.pop());
        if (seen.has(file)) continue;
        let source;
        try {
            source = readFileSync(file, 'utf8');
        } catch (cause) {
            throw new Error(`Markdown pipeline file is unreadable: ${file}`, {
                cause,
            });
        }
        seen.add(file);
        for (const specifier of localImports(source)) {
            pending.push(resolve(dirname(file), specifier));
        }
    }
    return [...seen].sort();
}

/**
 * A SHA-256 over the files' bytes, independent of their order. Each file is
 * length-prefixed, so a byte moved from one file to the next still changes
 * it.
 *
 * @param {string[]} files
 * @returns {string}
 */
export function pipelineDigest(files) {
    const hash = createHash('sha256');
    for (const file of [...files].sort()) {
        const bytes = readFileSync(file);
        hash.update(`${bytes.length}:`);
        hash.update(bytes);
    }
    return hash.digest('hex');
}

/**
 * @param {{ entries: (string | URL)[], extra?: (string | URL)[] }} options
 *   `entries` are the plugin modules; `extra` are hashed and watched as they
 *   are, without following imports (the lockfile).
 * @returns {import('astro').AstroIntegration}
 */
export default function markdownCacheKey({ entries, extra = [] }) {
    return {
        name: 'capsule:markdown-cache-key',
        hooks: {
            'astro:config:setup': ({ config, addWatchFile, logger }) => {
                const files = [...pipelineFiles(entries), ...extra.map(toPath)];
                const digest = pipelineDigest(files);
                config.markdown.processor.options[CACHE_KEY_OPTION] = digest;
                for (const file of files) addWatchFile(file);
                logger.debug(
                    `Markdown pipeline digest ${digest} over ${files.length} files: ${files.join(', ')}`,
                );
            },
        },
    };
}
