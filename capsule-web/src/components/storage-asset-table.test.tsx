// The storage asset table under Table v9: each sortable column orders the rows with the sort
// function its definition names (or `'auto'` resolves through the feature registry), and every
// row renders a cell for every column.
//
// The table renders to a string, as the rest of this suite does: there is no DOM here, so a
// sort is set through `initialSorting` rather than a header click.

import { describe, expect, test } from 'bun:test';
import type { SortingState } from '@tanstack/react-table';
import { renderToString } from 'react-dom/server';
import { IntlProvider } from 'react-intl';

import enMessages from '@/i18n/messages/en.json';
import { AssetList, AssetType, type StorageAsset } from './storage-asset-table';

function asset(
    id: string,
    fileName: string,
    path: string,
    size: number,
    day: number,
): StorageAsset {
    return {
        id,
        type: AssetType.Photo,
        fileName,
        path,
        size,
        date: new Date(Date.UTC(2023, 0, day)),
    };
}

// Each column orders these three differently, so a sort that read the wrong column, or fell
// back to input order, fails. The names sort naturally (`alphanumeric`: 2 < 10 < 30, where
// plain text puts IMG_10 first) and the sizes numerically (`basic`: where as strings
// "100000" < "9000").
const A = asset('a', 'IMG_10.jpg', '/c/IMG_10.jpg', 9_000, 2);
const B = asset('b', 'IMG_30.jpg', '/a/IMG_30.jpg', 100_000, 1);
const C = asset('c', 'IMG_2.jpg', '/b/IMG_2.jpg', 10_000, 3);
const ASSETS = [A, B, C];

function render(sorting: SortingState): string {
    return renderToString(
        <IntlProvider locale="en" messages={enMessages}>
            <AssetList assets={ASSETS} initialSorting={sorting} />
        </IntlProvider>,
    );
}

/** The ids of the rendered rows, top to bottom, read from their file-name cells. */
function rowOrder(html: string): string[] {
    const byName = new Map(ASSETS.map((a) => [a.fileName, a.id]));
    return [...html.matchAll(/<div class="font-medium">([^<]*)<\/div>/g)].map(
        (m) => byName.get(m[1] ?? '') ?? `unknown:${m[1]}`,
    );
}

describe('AssetList sorting', () => {
    test('keeps the input order when unsorted', () => {
        expect(rowOrder(render([]))).toEqual(['a', 'b', 'c']);
    });

    const cases: [column: string, ascending: string[]][] = [
        ['fileName', ['c', 'a', 'b']],
        ['path', ['b', 'c', 'a']],
        ['size', ['a', 'c', 'b']],
        ['date', ['b', 'a', 'c']],
    ];

    for (const [column, ascending] of cases) {
        test(`sorts by ${column} ascending`, () => {
            expect(rowOrder(render([{ id: column, desc: false }]))).toEqual(
                ascending,
            );
        });

        test(`sorts by ${column} descending`, () => {
            expect(rowOrder(render([{ id: column, desc: true }]))).toEqual(
                [...ascending].reverse(),
            );
        });
    }

    test('marks the sorted column and offers the thumbnail no sort', () => {
        const html = render([{ id: 'size', desc: false }]);
        expect(html.match(/role="button"/g)?.length).toBe(4);
        expect(html.match(/🔼/g)?.length).toBe(1);
    });

    test('renders a cell for every column in every row', () => {
        const html = render([]);
        expect(html.match(/<td /g)?.length).toBe(ASSETS.length * 5);
    });
});
