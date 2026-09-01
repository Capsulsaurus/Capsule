import { UploadCloud } from 'lucide-react';
import { useState } from 'react';
import { FormattedMessage } from 'react-intl';
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

export function UploadDialog({ children }: { children: React.ReactNode }) {
    const [isUploading, setIsUploading] = useState(false);
    const [progress, setProgress] = useState(0);

    const handleUpload = () => {
        setIsUploading(true);
        setProgress(0);
        const interval = setInterval(() => {
            setProgress((prev) => {
                if (prev >= 100) {
                    clearInterval(interval);
                    setIsUploading(false);
                    return 100;
                }
                return prev + 10;
            });
        }, 500);
    };

    return (
        <Dialog>
            <DialogTrigger asChild>{children}</DialogTrigger>
            <DialogContent className="sm:max-w-[425px]">
                <DialogHeader>
                    <DialogTitle>
                        <FormattedMessage id="upload.title" />
                    </DialogTitle>
                    <DialogDescription>
                        <FormattedMessage id="upload.description" />
                    </DialogDescription>
                </DialogHeader>
                <div className="grid gap-4 py-4">
                    <div
                        className="flex flex-col items-center justify-center border-2 border-dashed rounded-lg p-10 cursor-pointer hover:bg-muted/50 transition-colors"
                        onKeyUp={handleUpload}
                    >
                        <UploadCloud className="h-10 w-10 text-muted-foreground mb-4" />
                        <p className="text-sm text-muted-foreground text-center">
                            {isUploading ? (
                                <FormattedMessage id="upload.uploading" />
                            ) : (
                                <FormattedMessage id="upload.select_files" />
                            )}
                        </p>
                    </div>
                    {isUploading && (
                        // Progress bar placeholder since I might not have the component
                        <div className="w-full bg-secondary h-2 rounded-full overflow-hidden">
                            <div
                                className="bg-primary h-full transition-all duration-300"
                                style={{ width: `${progress}%` }}
                            />
                        </div>
                    )}
                </div>
                <DialogFooter>
                    <Button type="submit" disabled={isUploading}>
                        {isUploading ? (
                            <FormattedMessage id="common.cancel" />
                        ) : (
                            <FormattedMessage id="common.upload" />
                        )}
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
