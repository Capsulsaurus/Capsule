#!/usr/bin/env node
// Record a release's minor version in capsule-docs/public/versions.json, the
// list every docs version's switcher reads. Run by prepare-release.yml with
// plain node (no install), so it imports nothing but node built-ins and the
// pure helper it shares with the site.
//
// Usage: node capsule-docs/scripts/add-release.mjs X.Y.Z
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { addRelease } from '../src/lib/docs-version.mjs';

const file = join(
    dirname(fileURLToPath(import.meta.url)),
    '..',
    'public',
    'versions.json',
);
const release = process.argv[2];
const next = addRelease(JSON.parse(readFileSync(file, 'utf8')), release);
writeFileSync(file, `${JSON.stringify(next)}\n`);
process.stdout.write(`versions.json: ${next.versions.join(', ')}\n`);
