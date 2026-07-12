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
import { FormattedMessage } from 'react-intl';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils'; // Assuming cn exists, usually it does in shadcn

const sidebarItems = [
    { icon: Image, labelId: 'nav.photos', href: '/photos' },
    { icon: Compass, labelId: 'nav.explore', href: '/explore' },
    { icon: Share2, labelId: 'nav.sharing', href: '/sharing' },
];

const libraryItems = [
    { icon: Heart, labelId: 'nav.favorites', href: '/library/favorites' },
    { icon: Library, labelId: 'nav.albums', href: '/albums' },
    { icon: Archive, labelId: 'nav.archive', href: '/library/archive' },
    { icon: Trash2, labelId: 'nav.trash', href: '/library/trash' },
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
                                <FormattedMessage id={item.labelId} />
                            </Button>
                        </Link>
                    ))}
                </div>
            </div>
            <div className="px-3 py-2">
                <h2 className="mb-2 px-4 text-xs font-semibold tracking-tight text-muted-foreground uppercase">
                    <FormattedMessage id="nav.library" />
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
                                <FormattedMessage id={item.labelId} />
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
                        <FormattedMessage id="nav.storage_usage" />
                    </p>
                </div>
            </div>
        </aside>
    );
}
