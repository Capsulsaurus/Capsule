import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils'; // Assuming cn exists, usually it does in shadcn
import { Link, useRouterState } from '@tanstack/react-router';
import {
    Archive,
    Compass,
    Heart,
    Image,
    Library,
    Share2,
    Trash2,
} from 'lucide-react';

const sidebarItems = [
    { icon: Image, label: 'Photos', href: '/photos' },
    { icon: Compass, label: 'Explore', href: '/explore' },
    { icon: Share2, label: 'Sharing', href: '/sharing' },
];

const libraryItems = [
    { icon: Heart, label: 'Favorites', href: '/library/favorites' },
    { icon: Library, label: 'Albums', href: '/albums' },
    { icon: Archive, label: 'Archive', href: '/library/archive' },
    { icon: Trash2, label: 'Trash', href: '/library/trash' },
];

export function AppSidebar({ className }: { className?: string }) {
    const router = useRouterState();
    const currentPath = router.location.pathname;

    const isActive = (path: string) => {
        if (path === '/photos' && currentPath === '/') return true;
        return currentPath.startsWith(path);
    };

    return (
        <aside
            className={cn(
                'w-64 flex flex-col h-[calc(100vh-65px)] border-r bg-background py-4',
                className,
            )}
        >
            <div className="px-3 py-2">
                <div className="space-y-1">
                    {sidebarItems.map((item) => (
                        <Link to={item.href} key={item.href}>
                            <Button
                                variant={
                                    isActive(item.href) ? 'secondary' : 'ghost'
                                }
                                className="w-full justify-start"
                            >
                                <item.icon className="mr-2 h-4 w-4" />
                                {item.label}
                            </Button>
                        </Link>
                    ))}
                </div>
            </div>
            <div className="px-3 py-2">
                <h2 className="mb-2 px-4 text-xs font-semibold tracking-tight text-muted-foreground uppercase">
                    Library
                </h2>
                <div className="space-y-1">
                    {libraryItems.map((item) => (
                        <Link to={item.href} key={item.href}>
                            <Button
                                variant={
                                    isActive(item.href) ? 'secondary' : 'ghost'
                                }
                                className="w-full justify-start"
                            >
                                <item.icon className="mr-2 h-4 w-4" />
                                {item.label}
                            </Button>
                        </Link>
                    ))}
                </div>
            </div>
            <div className="mt-auto px-3 py-2">
                {/* Storage Meter Placeholder */}
                <div className="px-4 py-2">
                    <div className="h-2 w-full bg-secondary rounded-full overflow-hidden">
                        <div className="h-full bg-primary w-[45%]" />
                    </div>
                    <p className="text-xs text-muted-foreground mt-2">
                        15 GB used of 2 TB
                    </p>
                </div>
            </div>
        </aside>
    );
}
