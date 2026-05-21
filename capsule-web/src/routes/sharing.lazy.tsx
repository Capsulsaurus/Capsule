import { Button } from '@/components/ui/button';
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
    DialogTrigger,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { createLazyFileRoute } from '@tanstack/react-router';
import { Link as LinkIcon, Plus, Share2, Users } from 'lucide-react';
import { useState } from 'react';
import { toast } from 'sonner';

export const Route = createLazyFileRoute('/sharing')({
    component: Sharing,
});

function Sharing() {
    return (
        <div className="flex flex-col items-center justify-center p-20 text-center min-h-[50vh]">
            <div className="bg-muted/50 p-6 rounded-full mb-6">
                <Share2 className="w-12 h-12 text-muted-foreground" />
            </div>
            <h1 className="text-2xl font-bold mb-2">Sharing</h1>
            <p className="text-muted-foreground max-w-md mb-8">
                Share your photos and albums with friends and family. Shared
                content will appear here.
            </p>
            <CreateSharedAlbumDialog />
        </div>
    );
}

function CreateSharedAlbumDialog() {
    const [open, setOpen] = useState(false);
    const [title, setTitle] = useState('');
    const [linkSharing, setLinkSharing] = useState(true);
    const [collaborative, setCollaborative] = useState(false);

    const handleCreate = () => {
        // Logic to create album would go here
        console.log('Creating shared album:', {
            title,
            linkSharing,
            collaborative,
        });
        toast.success(`Shared album "${title}" created!`);
        setOpen(false);
        setTitle('');
    };

    return (
        <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
                <Button>Create Shared Album</Button>
            </DialogTrigger>
            <DialogContent className="sm:max-w-[425px]">
                <DialogHeader>
                    <DialogTitle>Create Shared Album</DialogTitle>
                    <DialogDescription>
                        Create a new album to share with friends and family.
                    </DialogDescription>
                </DialogHeader>
                <div className="grid gap-6 py-4">
                    <div className="grid gap-2">
                        <Label htmlFor="title">Album Title</Label>
                        <Input
                            id="title"
                            placeholder="e.g., Summer Vacation 2024"
                            value={title}
                            onChange={(e) => setTitle(e.target.value)}
                        />
                    </div>

                    <div className="flex items-center justify-between space-x-2">
                        <div className="flex flex-col space-y-1">
                            <Label
                                htmlFor="link-sharing"
                                className="flex items-center gap-2"
                            >
                                <LinkIcon className="w-4 h-4" /> Link Sharing
                            </Label>
                            <span className="text-xs text-muted-foreground">
                                Anyone with the link can view
                            </span>
                        </div>
                        <Switch
                            id="link-sharing"
                            checked={linkSharing}
                            onCheckedChange={setLinkSharing}
                        />
                    </div>

                    <div className="flex items-center justify-between space-x-2">
                        <div className="flex flex-col space-y-1">
                            <Label
                                htmlFor="collaborative"
                                className="flex items-center gap-2"
                            >
                                <Users className="w-4 h-4" /> Collaborative
                            </Label>
                            <span className="text-xs text-muted-foreground">
                                Allow others to add photos
                            </span>
                        </div>
                        <Switch
                            id="collaborative"
                            checked={collaborative}
                            onCheckedChange={setCollaborative}
                        />
                    </div>
                </div>
                <DialogFooter>
                    <Button variant="outline" onClick={() => setOpen(false)}>
                        Cancel
                    </Button>
                    <Button onClick={handleCreate} disabled={!title.trim()}>
                        Create Album
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
