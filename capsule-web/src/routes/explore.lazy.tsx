import { AssetGrid } from '@/components/asset-grid';
import { Card } from '@/components/ui/card';
import { mockAlbums, mockAssets } from '@/lib/mock-data';
import { createLazyFileRoute } from '@tanstack/react-router';
import { Link } from '@tanstack/react-router';

export const Route = createLazyFileRoute('/explore')({
    component: Explore,
});

function Explore() {
    // Just mixing some data for the 'explore' feel
    const featuredAssets = mockAssets.slice(0, 10);
    const featuredAlbums = mockAlbums.slice(0, 4);

    return (
        <div className="p-4 md:p-8 space-y-8">
            <section>
                <div className="flex items-center justify-between mb-4">
                    <h2 className="text-2xl font-bold">Featured Albums</h2>
                    <Link
                        to="/albums"
                        className="text-sm text-primary hover:underline"
                    >
                        View All
                    </Link>
                </div>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                    {featuredAlbums.map((album) => (
                        <Link
                            to="/albums/$id"
                            params={{ id: album.id }}
                            key={album.id}
                        >
                            <Card className="overflow-hidden h-full hover:shadow-lg transition-shadow border-none shadow-sm">
                                <div className="aspect-square bg-muted relative rounded-md overflow-hidden">
                                    <img
                                        src={album.coverUrl}
                                        alt={album.title}
                                        className="object-cover w-full h-full hover:scale-105 transition-transform duration-300"
                                        loading="lazy"
                                    />
                                    <div className="absolute inset-0 flex items-end bg-gradient-to-t from-black/60 to-transparent p-3">
                                        <div className="w-full">
                                            <h3 className="font-semibold text-white truncate">
                                                {album.title}
                                            </h3>
                                            <p className="text-xs text-white/80">
                                                {album.assetCount} items
                                            </p>
                                        </div>
                                    </div>
                                </div>
                            </Card>
                        </Link>
                    ))}
                </div>
            </section>

            <section>
                <h2 className="text-2xl font-bold mb-4">Trending Photos</h2>
                <AssetGrid
                    assets={featuredAssets}
                    onAssetClick={(asset) => console.log('Clicked', asset)}
                />
            </section>
        </div>
    );
}
