import { AssetGrid } from '@/components/asset-grid';
import { Button } from '@/components/ui/button';
import { mockAlbums, mockAssets } from '@/lib/mock-data';
import { createFileRoute } from '@tanstack/react-router';
import { MoreHorizontal, Play, Share2 } from 'lucide-react';

export const Route = createFileRoute('/albums/$id')({
    component: Album,
});

function Album() {
    const { id } = Route.useParams();
    // Find mock album or default
    const album = mockAlbums.find((a) => a.id === id) || mockAlbums[0];

    // Default to some assets
    const assets = mockAssets.slice(0, album.assetCount);

    return (
        <div className="flex flex-col h-full bg-background">
            <div className="relative w-full h-64 md:h-80 bg-muted shrink-0">
                <img
                    src={album.coverUrl}
                    alt={album.title}
                    className="w-full h-full object-cover opacity-60"
                />
                <div className="absolute inset-0 bg-gradient-to-t from-background to-transparent" />
                <div className="absolute bottom-0 left-0 p-6 md:p-10 w-full flex items-end justify-between">
                    <div>
                        <h1 className="text-4xl font-bold mb-2">
                            {album.title}
                        </h1>
                        <p className="text-muted-foreground">
                            {album.assetCount} items
                        </p>
                    </div>
                    <div className="flex gap-2">
                        <Button>
                            <Play className="w-4 h-4 mr-2" />
                            Slideshow
                        </Button>
                        <Button variant="secondary" size="icon">
                            <Share2 className="w-4 h-4" />
                        </Button>
                        <Button variant="ghost" size="icon">
                            <MoreHorizontal className="w-4 h-4" />
                        </Button>
                    </div>
                </div>
            </div>

            <div className="flex-1 min-h-0 relative">
                <AssetGrid
                    assets={assets}
                    onAssetClick={(a) => console.log('Clicked', a)}
                />
            </div>
        </div>
    );
}
