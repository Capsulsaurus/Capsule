import { AssetGrid } from '@/components/asset-grid';
import { Button } from '@/components/ui/button';
import { mockAssets } from '@/lib/mock-data';
import { createLazyFileRoute } from '@tanstack/react-router';
import { Trash2 } from 'lucide-react';

export const Route = createLazyFileRoute('/library/trash')({
    component: Trash,
});

function Trash() {
    const trashAssets = mockAssets.slice(25, 30);

    return (
        <div className="p-4 pt-2">
            <header className="mb-6 flex items-center justify-between">
                <div>
                    <div className="flex items-center gap-3 mb-2">
                        <div className="bg-zinc-100 dark:bg-zinc-800 p-2 rounded-full">
                            <Trash2 className="w-5 h-5 text-zinc-600 dark:text-zinc-400" />
                        </div>
                        <h1 className="text-2xl font-bold">Trash</h1>
                    </div>
                    <p className="text-muted-foreground ml-11">
                        Items in trash will be permanently deleted after 30
                        days.
                    </p>
                </div>
                {trashAssets.length > 0 && (
                    <Button variant="destructive" size="sm">
                        Empty Trash
                    </Button>
                )}
            </header>

            {trashAssets.length > 0 ? (
                <AssetGrid
                    assets={trashAssets}
                    onAssetClick={(asset) => console.log('Clicked', asset)}
                />
            ) : (
                <div className="flex flex-col items-center justify-center p-20 text-muted-foreground">
                    <Trash2 className="w-12 h-12 mb-4 opacity-20" />
                    <p>Trash is empty</p>
                </div>
            )}
        </div>
    );
}
