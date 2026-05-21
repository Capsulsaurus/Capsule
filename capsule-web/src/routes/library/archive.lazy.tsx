import { AssetGrid } from '@/components/asset-grid';
import { mockAssets } from '@/lib/mock-data';
import { createLazyFileRoute } from '@tanstack/react-router';
import { Archive as ArchiveIcon } from 'lucide-react';

export const Route = createLazyFileRoute('/library/archive')({
    component: Archive,
});

function Archive() {
    const archivedAssets = mockAssets.slice(15, 25);

    return (
        <div className="p-4 pt-2">
            <header className="mb-6">
                <div className="flex items-center gap-3 mb-2">
                    <div className="bg-amber-100 dark:bg-amber-900/20 p-2 rounded-full">
                        <ArchiveIcon className="w-5 h-5 text-amber-600 dark:text-amber-400" />
                    </div>
                    <h1 className="text-2xl font-bold">Archive</h1>
                </div>
                <p className="text-muted-foreground ml-11">
                    Archived photos are hidden from your main photos view.
                </p>
            </header>

            {archivedAssets.length > 0 ? (
                <AssetGrid
                    assets={archivedAssets}
                    onAssetClick={(asset) => console.log('Clicked', asset)}
                />
            ) : (
                <div className="flex flex-col items-center justify-center p-20 text-muted-foreground">
                    <ArchiveIcon className="w-12 h-12 mb-4 opacity-20" />
                    <p>No archived photos</p>
                </div>
            )}
        </div>
    );
}
