import { createLazyFileRoute } from '@tanstack/react-router';
import { FileText, Image, RefreshCw, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import {
    AssetList,
    AssetType,
    formatFileSize,
    type StorageAsset,
} from '@/components/storage-asset-table';
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogHeader,
    DialogTitle,
} from '@/components/ui/dialog';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { cn } from '@/lib/utils';

export const Route = createLazyFileRoute('/storage')({
    component: () => <Storage />,
});

// Mock Data Types
type StorageStats = {
    totalPhotos: number;
    totalAlbums: number;
    usedStorage: number;
    usedStoragePhotos: number;
    usedStorageVideos: number;
    usedStorageSidecar: number;
    totalStorage: number;
    usedStorageTrash: number;
    usedStorageSimilarAssets: number;
    usedStorageLargeFiles: number;
};

const mockStats: StorageStats = {
    totalPhotos: 1234,
    totalAlbums: 42,
    usedStorage: 45000000000, // 45 GB
    usedStoragePhotos: 25000000000, // 25 GB
    usedStorageVideos: 15000000000, // 15 GB
    usedStorageSidecar: 500000000, // 500 MB
    totalStorage: 100000000000, // 100 GB
    usedStorageTrash: 2000000000, // 2 GB
    usedStorageSimilarAssets: 1500000000, // 1.5 GB
    usedStorageLargeFiles: 3000000000, // 3 GB
};

const mockStorageAssets: StorageAsset[] = Array.from({ length: 20 }).map(
    (_, i) => ({
        id: `asset-${i}`,
        type: i % 3 === 0 ? AssetType.Video : AssetType.Photo,
        fileName: `IMG_${20230000 + i}.${i % 3 === 0 ? 'mp4' : 'jpg'}`,
        path: `/DCIM/100CANON/IMG_${20230000 + i}.${i % 3 === 0 ? 'mp4' : 'jpg'}`,
        size: i % 3 === 0 ? 50000000 + i * 1000 : 4000000 + i * 1000,
        date: new Date(2023, 5, i + 1),
        thumbnail: `https://picsum.photos/seed/${i}/100/100`,
    }),
);

type Suggestion = 'trash-files' | 'similar-assets' | 'large-files';

function Storage() {
    const intl = useIntl();
    // Mocking useQuery
    const fetching = false;
    const error = null;
    const stats = mockStats;

    const [activeTab, setActiveTab] = useState<Suggestion>('trash-files');

    const [previewAssetId] = useState<string | undefined>(undefined);
    const [previewOpen, setPreviewOpen] = useState(false);

    // Icon mapping for cleanup suggestions
    const getIconForSuggestion = (id: string) => {
        switch (id) {
            case 'trash-files':
                return <Trash2 size={28} className="text-gray-700" />;
            case 'similar-assets':
                return <Image size={28} className="text-gray-700" />;
            case 'large-files':
                return <FileText size={28} className="text-gray-700" />;
            default:
                return <FileText size={28} className="text-gray-700" />;
        }
    };

    return (
        <Dialog open={previewOpen} onOpenChange={setPreviewOpen}>
            <div className="max-w-6xl mx-auto p-6">
                {/* Storage Usage Overview */}
                <div className="mb-10">
                    <h1 className="text-2xl font-medium text-gray-800 mb-4">
                        <FormattedMessage id="storage.title" />
                    </h1>
                    {fetching ? (
                        <div>
                            <FormattedMessage id="common.loading" />
                        </div>
                    ) : error ? (
                        <div>
                            <FormattedMessage id="storage.error" />
                        </div>
                    ) : !stats ? (
                        <div>
                            <FormattedMessage id="storage.no_data" />
                        </div>
                    ) : (
                        (() => {
                            const storageData = {
                                totalStorage: stats.totalStorage, // in bytes
                                usedStorage: stats.usedStorage, // in bytes
                                categories: [
                                    {
                                        nameId: 'storage.category.photos',
                                        size: stats.usedStoragePhotos,
                                        color: 'bg-red-500',
                                    },
                                    {
                                        nameId: 'storage.category.videos',
                                        size: stats.usedStorageVideos,
                                        color: 'bg-blue-500',
                                    },
                                    {
                                        nameId: 'storage.category.sidecar',
                                        size: stats.usedStorageSidecar,
                                        color: 'bg-purple-600',
                                    },
                                ],
                                cleanupSuggestions: [
                                    {
                                        id: 'trash-files',
                                        titleId: 'storage.suggestion.trash',
                                        size: stats.usedStorageTrash,
                                    },
                                    {
                                        id: 'similar-assets',
                                        titleId: 'storage.suggestion.similar',
                                        size: stats.usedStorageSimilarAssets,
                                    },
                                    {
                                        id: 'large-files',
                                        titleId: 'storage.suggestion.large',
                                        size: stats.usedStorageLargeFiles,
                                    },
                                ],
                            };

                            // Calculate percentage of storage used
                            const totalPercentageUsed =
                                (storageData.usedStorage /
                                    storageData.totalStorage) *
                                100;

                            return (
                                <>
                                    <h2 className="text-xl font-medium text-gray-800 mb-4">
                                        <FormattedMessage
                                            id="storage.usage_summary"
                                            values={{
                                                percent:
                                                    totalPercentageUsed.toFixed(
                                                        0,
                                                    ),
                                                used: formatFileSize(
                                                    storageData.usedStorage,
                                                ),
                                                total: formatFileSize(
                                                    storageData.totalStorage,
                                                ),
                                            }}
                                        />
                                    </h2>
                                    <p className="text-gray-600 mb-6">
                                        <FormattedMessage id="storage.cleanup_hint" />
                                    </p>

                                    {/* Progress Bar */}
                                    <div className="h-2 w-full rounded-full overflow-hidden flex">
                                        {storageData.categories.map(
                                            (category) => (
                                                <div
                                                    key={category.nameId}
                                                    className={`h-full ${category.color}`}
                                                    style={{
                                                        width: `${(category.size / storageData.totalStorage) * 100}%`,
                                                    }}
                                                />
                                            ),
                                        )}
                                        <div
                                            className="h-full bg-gray-200"
                                            style={{
                                                width: `${100 - totalPercentageUsed}%`,
                                            }}
                                        />
                                    </div>

                                    {/* Legend */}
                                    <div className="flex flex-wrap mt-4 justify-between">
                                        <div className="flex flex-wrap">
                                            {storageData.categories.map(
                                                (category) => (
                                                    <div
                                                        key={category.nameId}
                                                        className="flex items-center me-6 mb-2"
                                                    >
                                                        <div
                                                            className={cn(
                                                                'w-3 h-3 rounded-full',
                                                                category.color,
                                                                'me-2',
                                                            )}
                                                        />
                                                        <span className="text-sm text-gray-700">
                                                            <FormattedMessage
                                                                id="storage.category_legend"
                                                                values={{
                                                                    name: intl.formatMessage(
                                                                        {
                                                                            id: category.nameId,
                                                                        },
                                                                    ),
                                                                    size: formatFileSize(
                                                                        category.size,
                                                                    ),
                                                                }}
                                                            />
                                                        </span>
                                                    </div>
                                                ),
                                            )}
                                        </div>
                                        <div className="flex items-center mb-2">
                                            <div
                                                className={cn(
                                                    'w-3 h-3 rounded-full',
                                                    'bg-gray-200',
                                                    'me-2',
                                                )}
                                            />
                                            <span className="text-sm text-gray-700">
                                                <FormattedMessage
                                                    id="storage.free_space"
                                                    values={{
                                                        size: formatFileSize(
                                                            storageData.totalStorage -
                                                                storageData.usedStorage,
                                                        ),
                                                    }}
                                                />
                                            </span>
                                        </div>
                                    </div>

                                    {/* Clean Up Suggestions */}
                                    <div>
                                        <h2 className="text-xl font-medium text-gray-800 mb-6">
                                            <FormattedMessage id="storage.cleanup_title" />
                                        </h2>
                                        <Tabs
                                            value={activeTab}
                                            onValueChange={(value) =>
                                                setActiveTab(
                                                    value as Suggestion,
                                                )
                                            }
                                            className="w-full"
                                        >
                                            <TabsList className="mb-4">
                                                {storageData.cleanupSuggestions.map(
                                                    (suggestion) => (
                                                        <TabsTrigger
                                                            key={suggestion.id}
                                                            value={
                                                                suggestion.id
                                                            }
                                                        >
                                                            <div className="flex items-center gap-2">
                                                                {getIconForSuggestion(
                                                                    suggestion.id,
                                                                )}
                                                                <FormattedMessage
                                                                    id={
                                                                        suggestion.titleId
                                                                    }
                                                                />
                                                            </div>
                                                        </TabsTrigger>
                                                    ),
                                                )}
                                            </TabsList>
                                            {storageData.cleanupSuggestions.map(
                                                (suggestion) => {
                                                    // Mock asset fetching
                                                    const assetFetching = false;
                                                    const assetError = null;
                                                    const assetData =
                                                        mockStorageAssets;

                                                    return (
                                                        <TabsContent
                                                            key={suggestion.id}
                                                            value={
                                                                suggestion.id
                                                            }
                                                        >
                                                            <div className="flex flex-col">
                                                                {assetFetching ? (
                                                                    <div>
                                                                        <FormattedMessage id="common.loading" />
                                                                    </div>
                                                                ) : assetError ? (
                                                                    <div>
                                                                        <FormattedMessage id="storage.error_mock" />
                                                                    </div>
                                                                ) : assetData ? (
                                                                    <AssetList
                                                                        assets={
                                                                            assetData
                                                                        }
                                                                    />
                                                                ) : (
                                                                    <div>
                                                                        <FormattedMessage id="storage.no_data" />
                                                                    </div>
                                                                )}
                                                                <div className="flex justify-end mb-4">
                                                                    <button
                                                                        type="button"
                                                                        onClick={() =>
                                                                            console.info(
                                                                                'Refresh',
                                                                            )
                                                                        }
                                                                        className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-700 bg-white border border-gray-300 rounded-md hover:bg-gray-50 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-blue-500"
                                                                    >
                                                                        <RefreshCw className="h-4 w-4" />
                                                                        <FormattedMessage id="common.refresh" />
                                                                    </button>
                                                                </div>
                                                            </div>
                                                        </TabsContent>
                                                    );
                                                },
                                            )}
                                        </Tabs>
                                    </div>
                                </>
                            );
                        })()
                    )}
                </div>
            </div>
            <DialogContent>
                <DialogHeader>
                    <DialogTitle>
                        <FormattedMessage id="storage.preview" />
                    </DialogTitle>
                    <DialogDescription>
                        {previewAssetId}
                        {/* TODO: Add preview */}
                    </DialogDescription>
                </DialogHeader>
            </DialogContent>
        </Dialog>
    );
}
