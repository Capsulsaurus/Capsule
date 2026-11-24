// The sortable asset table on the storage screen's cleanup tabs.

import {
    createColumnHelper,
    createSortedRowModel,
    flexRender,
    rowSortingFeature,
    type SortingState,
    sortFn_alphanumeric,
    sortFn_basic,
    sortFn_datetime,
    sortFn_text,
    tableFeatures,
    useTable,
} from '@tanstack/react-table';
import { filesize } from 'filesize';
import { FileText, FileVideo, Image } from 'lucide-react';
import { useMemo, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';

// Mock AssetType Enum
export enum AssetType {
    Photo = 'PHOTO',
    Video = 'VIDEO',
    Sidecar = 'SIDECAR',
}

// Asset type definition
export type StorageAsset = {
    id: string;
    type: AssetType;
    fileName: string;
    path: string;
    size: number;
    date: Date;
    thumbnail?: string;
};

export function formatFileSize(size: number) {
    return filesize(size, {
        base: 2,
        standard: 'jedec',
    });
}

// The asset table's features: sorting, plus every sort function a column names or `'auto'`
// picks. Table v9 resolves a function name only from this registry, so `alphanumeric` and
// `text` stay registered for the string columns that rely on `'auto'`.
const assetTableFeatures = tableFeatures({
    rowSortingFeature,
    sortedRowModel: createSortedRowModel(),
    sortFns: {
        alphanumeric: sortFn_alphanumeric,
        basic: sortFn_basic,
        datetime: sortFn_datetime,
        text: sortFn_text,
    },
});

const columnHelper = createColumnHelper<
    typeof assetTableFeatures,
    StorageAsset
>();

/**
 * The assets as a table sortable by every column but the thumbnail. `initialSorting` is the
 * order it opens in; a header click changes it from there.
 */
export function AssetList({
    assets,
    initialSorting = [],
}: {
    assets: StorageAsset[];
    initialSorting?: SortingState;
}) {
    const intl = useIntl();

    // Stable across renders, as Table v9 requires: rebuilt only when the locale changes.
    const columns = useMemo(
        () =>
            columnHelper.columns([
                columnHelper.accessor('thumbnail', {
                    header: '',
                    cell: (info) => (
                        <div className="flex items-center justify-center w-12 h-12">
                            {info.getValue() ? (
                                <div className="w-10 h-10 bg-blue-100 rounded overflow-hidden">
                                    <img
                                        src={info.getValue()}
                                        alt={intl.formatMessage(
                                            { id: 'storage.thumbnail_alt' },
                                            {
                                                fileName:
                                                    info.row.original.fileName,
                                            },
                                        )}
                                        className="w-full h-full object-cover"
                                    />
                                </div>
                            ) : (
                                <div className="w-10 h-10 bg-gray-100 rounded flex items-center justify-center">
                                    {info.row.original.type ===
                                    AssetType.Photo ? (
                                        <Image
                                            className="size-6 text-blue-500"
                                            aria-label={intl.formatMessage(
                                                {
                                                    id: 'storage.image_file_alt',
                                                },
                                                {
                                                    fileName:
                                                        info.row.original
                                                            .fileName,
                                                },
                                            )}
                                        />
                                    ) : info.row.original.type ===
                                      AssetType.Video ? (
                                        <FileVideo
                                            className="size-6 text-red-500"
                                            aria-label={intl.formatMessage(
                                                {
                                                    id: 'storage.video_file_alt',
                                                },
                                                {
                                                    fileName:
                                                        info.row.original
                                                            .fileName,
                                                },
                                            )}
                                        />
                                    ) : info.row.original.type ===
                                      AssetType.Sidecar ? (
                                        <FileText
                                            className="size-6 text-purple-500"
                                            aria-label={intl.formatMessage(
                                                {
                                                    id: 'storage.sidecar_file_alt',
                                                },
                                                {
                                                    fileName:
                                                        info.row.original
                                                            .fileName,
                                                },
                                            )}
                                        />
                                    ) : null}
                                </div>
                            )}
                        </div>
                    ),
                    enableSorting: false,
                }),
                columnHelper.accessor('fileName', {
                    header: () => (
                        <FormattedMessage id="storage.col_filename" />
                    ),
                    cell: (info) => (
                        <div className="font-medium">{info.getValue()}</div>
                    ),
                }),
                columnHelper.accessor('path', {
                    header: () => <FormattedMessage id="storage.col_path" />,
                    cell: (info) => (
                        <div className="text-gray-500 text-sm">
                            {info.getValue()}
                        </div>
                    ),
                }),
                columnHelper.accessor('size', {
                    header: () => (
                        <div className="text-end">
                            <FormattedMessage id="storage.col_size" />
                        </div>
                    ),
                    cell: (info) => (
                        <div className="text-end">
                            {formatFileSize(info.getValue())}
                        </div>
                    ),
                    sortFn: 'basic',
                }),
                columnHelper.accessor('date', {
                    header: () => (
                        <div className="text-end">
                            <FormattedMessage id="storage.col_date" />
                        </div>
                    ),
                    cell: (info) => (
                        <div className="text-end">
                            {intl.formatDate(info.getValue())}
                        </div>
                    ),
                    sortFn: 'datetime',
                }),
            ]),
        [intl],
    );

    const [sorting, setSorting] = useState<SortingState>(initialSorting);

    const table = useTable({
        features: assetTableFeatures,
        data: assets,
        columns,
        state: {
            sorting,
        },
        onSortingChange: setSorting,
    });

    if (assets.length === 0) {
        return (
            <div>
                <FormattedMessage id="storage.empty_list" />
            </div>
        );
    }

    return (
        <div className="rounded-md border">
            <table className="w-full">
                <thead>
                    {table.getHeaderGroups().map((headerGroup) => (
                        <tr
                            key={headerGroup.id}
                            className="border-b bg-gray-50"
                        >
                            {headerGroup.headers.map((header) => (
                                <th
                                    key={header.id}
                                    className="px-4 py-3 text-start text-sm font-medium text-gray-700"
                                    onClick={header.column.getToggleSortingHandler()}
                                    onKeyDown={(e) => {
                                        if (
                                            e.key === 'Enter' ||
                                            e.key === ' '
                                        ) {
                                            header.column.getToggleSortingHandler();
                                        }
                                    }}
                                    style={{
                                        cursor: header.column.getCanSort()
                                            ? 'pointer'
                                            : 'default',
                                    }}
                                    tabIndex={
                                        header.column.getCanSort()
                                            ? 0
                                            : undefined
                                    }
                                    role={
                                        header.column.getCanSort()
                                            ? 'button'
                                            : undefined
                                    }
                                >
                                    <div className="flex items-center">
                                        {flexRender(
                                            header.column.columnDef.header,
                                            header.getContext(),
                                        )}
                                        {header.column.getCanSort() && (
                                            <span className="ms-1">
                                                {header.column.getIsSorted() ===
                                                'asc'
                                                    ? '🔼'
                                                    : header.column.getIsSorted() ===
                                                        'desc'
                                                      ? '🔽'
                                                      : '⏺️'}
                                            </span>
                                        )}
                                    </div>
                                </th>
                            ))}
                        </tr>
                    ))}
                </thead>
                <tbody>
                    {table.getRowModel().rows.map((row) => (
                        <tr key={row.id} className="border-b hover:bg-gray-50">
                            {row.getAllCells().map((cell) => (
                                <td
                                    key={cell.id}
                                    className="px-4 py-3 text-sm"
                                    // onKeyDown={() => {
                                    //   setPreviewAssetId(row.id);
                                    //   setPreviewOpen(true);
                                    // }} // TODO: Fix this
                                >
                                    {flexRender(
                                        cell.column.columnDef.cell,
                                        cell.getContext(),
                                    )}
                                </td>
                            ))}
                        </tr>
                    ))}
                </tbody>
            </table>
        </div>
    );
}
