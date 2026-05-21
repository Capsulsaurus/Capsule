import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow,
} from '@/components/ui/table';
import { filesize } from 'filesize';
import { toast } from 'sonner';

import { Link, createLazyFileRoute } from '@tanstack/react-router';
import { Album, HardDrive, Image, Loader2, RotateCw } from 'lucide-react';

import { mockAlbums, mockAssets } from '@/lib/mock-data';
import { useState } from 'react';

export const Route = createLazyFileRoute('/dashboard')({
    component: () => <Dashboard />,
});

const Dashboard = () => {
    const [fetching, setFetching] = useState(false);

    // Calculate mock stats
    const stats = {
        totalPhotos: mockAssets.length,
        totalAlbums: mockAlbums.length,
        usedStorage: mockAssets.length * 1024 * 1024 * 5, // Mock 5MB per photo
    };

    return (
        <div className="flex flex-col w-full min-h-screen bg-background">
            <main className="flex flex-col gap-8 p-4 md:p-10">
                <div className="flex items-center justify-between gap-4">
                    <h1 className="text-3xl font-bold">Dashboard</h1>
                    <button
                        type="button"
                        onClick={async () => {
                            setFetching(true);
                            await new Promise((resolve) =>
                                setTimeout(resolve, 1000),
                            );
                            setFetching(false);
                            toast.success('Data fetched successfully');
                        }}
                        className="inline-flex items-center justify-center rounded-md text-sm font-medium ring-offset-background transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 border border-input bg-background hover:bg-accent hover:text-accent-foreground h-10 w-10"
                    >
                        {fetching ? (
                            <Loader2 className="h-4 w-4 animate-spin" />
                        ) : (
                            <RotateCw className="h-4 w-4" />
                        )}
                    </button>
                </div>
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
                    <Link to="/photos">
                        <Card className="transition-colors hover:bg-muted/50">
                            <CardHeader className="flex flex-row items-center justify-between pb-2">
                                <CardTitle className="text-sm font-medium">
                                    Photos
                                </CardTitle>
                                <Image className="w-4 h-4 text-muted-foreground" />
                            </CardHeader>
                            <CardContent>
                                <div className="text-2xl font-bold">
                                    {stats.totalPhotos.toLocaleString()}
                                </div>
                            </CardContent>
                        </Card>
                    </Link>

                    <Link to="/albums">
                        <Card className="transition-colors hover:bg-muted/50">
                            <CardHeader className="flex flex-row items-center justify-between pb-2">
                                <CardTitle className="text-sm font-medium">
                                    Albums
                                </CardTitle>
                                <Album className="w-4 h-4 text-muted-foreground" />
                            </CardHeader>
                            <CardContent>
                                <div className="text-2xl font-bold">
                                    {stats.totalAlbums.toLocaleString()}
                                </div>
                            </CardContent>
                        </Card>
                    </Link>

                    <Link to="/storage">
                        <Card className="transition-colors hover:bg-muted/50">
                            <CardHeader className="flex flex-row items-center justify-between pb-2">
                                <CardTitle className="text-sm font-medium">
                                    Storage Used
                                </CardTitle>
                                <HardDrive className="w-4 h-4 text-muted-foreground" />
                            </CardHeader>
                            <CardContent>
                                <div className="text-2xl font-bold">
                                    {filesize(stats.usedStorage, {
                                        base: 2,
                                        standard: 'jedec',
                                    })}
                                </div>
                            </CardContent>
                        </Card>
                    </Link>
                </div>

                <h2 className="text-2xl">Recent Activity (Mock)</h2>
                <Card>
                    <Table>
                        <TableHeader>
                            <TableRow>
                                <TableHead className="w-[200px]">
                                    Timestamp
                                </TableHead>
                                <TableHead>Activity</TableHead>
                            </TableRow>
                        </TableHeader>
                        <TableBody>
                            <TableRow>
                                <TableCell>Just now</TableCell>
                                <TableCell>Mock activity log...</TableCell>
                            </TableRow>
                        </TableBody>
                    </Table>
                </Card>
            </main>
        </div>
    );
};
