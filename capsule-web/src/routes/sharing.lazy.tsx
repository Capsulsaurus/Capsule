import { createLazyFileRoute } from '@tanstack/react-router';
import { Link as LinkIcon, Share2, Users } from 'lucide-react';
import { useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { toast } from 'sonner';
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

export const Route = createLazyFileRoute('/sharing')({
    component: Sharing,
});

function Sharing() {
    return (
        <div className="flex flex-col items-center justify-center p-20 text-center min-h-[50vh]">
            <div className="bg-muted/50 p-6 rounded-full mb-6">
                <Share2 className="w-12 h-12 text-muted-foreground" />
            </div>
            <h1 className="text-2xl font-bold mb-2">
                <FormattedMessage id="nav.sharing" />
            </h1>
            <p className="text-muted-foreground max-w-md mb-8">
                <FormattedMessage id="sharing.description" />
            </p>
            <CreateSharedAlbumDialog />
        </div>
    );
}

function CreateSharedAlbumDialog() {
    const intl = useIntl();
    const [open, setOpen] = useState(false);
    const [title, setTitle] = useState('');
    const [linkSharing, setLinkSharing] = useState(true);
    const [collaborative, setCollaborative] = useState(false);

    const handleCreate = () => {
        // Logic to create album would go here
        toast.success(intl.formatMessage({ id: 'sharing.created' }, { title }));
        setOpen(false);
        setTitle('');
    };

    return (
        <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
                <Button>
                    <FormattedMessage id="sharing.create_button" />
                </Button>
            </DialogTrigger>
            <DialogContent className="sm:max-w-[425px]">
                <DialogHeader>
                    <DialogTitle>
                        <FormattedMessage id="sharing.create_button" />
                    </DialogTitle>
                    <DialogDescription>
                        <FormattedMessage id="sharing.dialog_description" />
                    </DialogDescription>
                </DialogHeader>
                <div className="grid gap-6 py-4">
                    <div className="grid gap-2">
                        <Label htmlFor="title">
                            <FormattedMessage id="sharing.album_title" />
                        </Label>
                        <Input
                            id="title"
                            placeholder={intl.formatMessage({
                                id: 'sharing.album_title_placeholder',
                            })}
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
                                <LinkIcon className="w-4 h-4" />{' '}
                                <FormattedMessage id="sharing.link_sharing" />
                            </Label>
                            <span className="text-xs text-muted-foreground">
                                <FormattedMessage id="sharing.link_sharing_hint" />
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
                                <Users className="w-4 h-4" />{' '}
                                <FormattedMessage id="sharing.collaborative" />
                            </Label>
                            <span className="text-xs text-muted-foreground">
                                <FormattedMessage id="sharing.collaborative_hint" />
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
                        <FormattedMessage id="common.cancel" />
                    </Button>
                    <Button onClick={handleCreate} disabled={!title.trim()}>
                        <FormattedMessage id="sharing.create_album" />
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
