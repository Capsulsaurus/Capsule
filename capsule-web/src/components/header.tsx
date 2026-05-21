import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Button } from '@/components/ui/button';
import {
    DropdownMenu,
    DropdownMenuContent,
    DropdownMenuItem,
    DropdownMenuSeparator,
    DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';

import { useAuth } from '@/lib/auth-context';
import { APP_NAME } from '@/lib/constant';
import { Link, useNavigate } from '@tanstack/react-router';
import { BellIcon, MountainIcon, UploadIcon } from 'lucide-react';
import { ModeToggle } from './ui/mode-toggle';
import { UploadDialog } from './upload-dialog';

export const Header = () => {
    const { user, isAuthenticated, logout } = useAuth();
    const navigate = useNavigate();

    const initials = user
        ? (user.name || user.username)
              .split(' ')
              .map((w) => w[0])
              .join('')
              .toUpperCase()
              .slice(0, 2)
        : '?';

    async function handleLogout() {
        await logout();
        navigate({ to: '/login' });
    }

    return (
        <header className="w-full bg-background px-4 py-3 shadow-sm dark:bg-muted border-b">
            <div className="flex items-center justify-between w-full">
                <Link to="/" className="flex items-center gap-2">
                    <MountainIcon className="size-6 text-primary" />
                    <span className="text-lg font-bold text-primary">
                        {APP_NAME}
                    </span>
                </Link>
                <div className="flex flex-1 items-center justify-center px-4">
                    <Input
                        type="text"
                        placeholder="Search your photos..."
                        className="w-full max-w-md rounded-md bg-muted px-4 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                    />
                </div>
                <div className="flex items-center gap-2">
                    <UploadDialog>
                        <Button variant="ghost" size="icon">
                            <UploadIcon className="h-5 w-5" />
                            <span className="sr-only">Upload</span>
                        </Button>
                    </UploadDialog>
                    <Button variant="ghost" size="icon">
                        <BellIcon className="h-5 w-5" />
                        <span className="sr-only">Notifications</span>
                    </Button>
                    <ModeToggle />
                    <div className="w-2" />
                    {isAuthenticated ? (
                        <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                                <Avatar className="h-8 w-8 cursor-pointer">
                                    {user?.profile_image_url && (
                                        <AvatarImage
                                            src={user.profile_image_url}
                                            alt={user.name}
                                        />
                                    )}
                                    <AvatarFallback>{initials}</AvatarFallback>
                                </Avatar>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end">
                                {user && (
                                    <>
                                        <div className="px-2 py-1.5 text-sm font-medium">
                                            {user.name}
                                        </div>
                                        <div className="px-2 pb-1.5 text-xs text-muted-foreground">
                                            {user.email}
                                        </div>
                                        <DropdownMenuSeparator />
                                    </>
                                )}
                                <DropdownMenuItem asChild>
                                    <Link to="/settings">Profile</Link>
                                </DropdownMenuItem>
                                <DropdownMenuItem asChild>
                                    <Link to="/settings/security">
                                        Security
                                    </Link>
                                </DropdownMenuItem>
                                <DropdownMenuSeparator />
                                <DropdownMenuItem
                                    className="text-destructive"
                                    onSelect={handleLogout}
                                >
                                    Logout
                                </DropdownMenuItem>
                            </DropdownMenuContent>
                        </DropdownMenu>
                    ) : (
                        <Link to="/login">
                            <Button size="sm">Sign in</Button>
                        </Link>
                    )}
                </div>
            </div>
        </header>
    );
};
