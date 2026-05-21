import { Card, CardContent } from '@/components/ui/card';
import { mockAlbums } from '@/lib/mock-data';
import { Link, createFileRoute } from '@tanstack/react-router';

// TODO: Implement
export const Route = createFileRoute('/albums/')({
    staleTime: Number.POSITIVE_INFINITY,
    component: () => <Albums />,
});

const Albums = () => {
    return (
        <div className="p-4 md:p-8">
            <h1 className="text-2xl font-bold mb-6">Albums</h1>
            <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 gap-4">
                {mockAlbums.map((album) => (
                    <Link
                        to="/albums/$id"
                        params={{ id: album.id }}
                        key={album.id}
                    >
                        <Card className="overflow-hidden h-full hover:shadow-lg transition-shadow border-none shadow-sm">
                            <div className="aspect-square bg-muted relative">
                                <img
                                    src={album.coverUrl}
                                    alt={album.title}
                                    className="object-cover w-full h-full hover:scale-105 transition-transform duration-300"
                                    loading="lazy"
                                />
                            </div>
                            <CardContent className="p-3">
                                <h3 className="font-semibold truncate">
                                    {album.title}
                                </h3>
                                <p className="text-xs text-muted-foreground">
                                    {album.assetCount} items
                                </p>
                            </CardContent>
                        </Card>
                    </Link>
                ))}
            </div>
            {/* Create Album Placeholder Card */}
            {/* Can add later */}
        </div>
    );
};
