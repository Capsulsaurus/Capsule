// Guest drop uploader (slice S-D3): `https://server.tld/u/{opaque-id}#{drop_pubkey}`.
//
// No auth, no account. The `#{drop_pubkey}` fragment (the link's Drop Key, plus any passphrase
// salt/params) stays in the browser — it is never sent to the server. Each selected file is sealed
// entirely client-side via the `capsule-wasm` module (fresh key `K`, STREAM-encrypted, `K`
// encapsulated to the Drop Key) and streamed to the drop endpoints. This surface is strictly
// contribute-only: it can add files to the owner's staging inbox and nothing more — it never lists
// or reads anything back (SSoT: the Web Upload design doc — Contribute-only). A not-found /
// revoked / expired link is one indistinguishable server `404`, surfaced as a single generic
// message.

import { createFileRoute } from '@tanstack/react-router';
import {
    CheckCircle2Icon,
    LockIcon,
    ShieldCheckIcon,
    UploadIcon,
} from 'lucide-react';
import type React from 'react';
import { useCallback, useMemo, useRef, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { Button } from '@/components/ui/button';
import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle,
} from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { parseDropFragment } from '@/lib/drop-fragment';
import { dropPassphraseProof, initDropWasm, sealAsset } from '@/lib/drop-seal';
import {
    type DropFailureCode,
    DropUploadError,
    uploadDrop,
} from '@/lib/drop-upload';

export const Route = createFileRoute('/u/$opaqueId')({
    component: DropUploader,
});

/** Base origin for the drop serve endpoints. Empty (same-origin) in production — the home server
 *  serves both this uploader and `/u/…`; overridable for split dev deployments. */
const DROP_BASE = import.meta.env.PUBLIC_SHARE_URL ?? '';

/** The i18n catalog key for each upload failure class. */
const FAILURE_KEY: Record<DropFailureCode, string> = {
    unavailable: 'drop.error.unavailable',
    rate_limited: 'drop.error.rate_limited',
    too_large: 'drop.error.too_large',
    quota: 'drop.error.quota',
    cap: 'drop.error.cap',
    passphrase: 'drop.error.passphrase',
    unsupported_type: 'drop.error.unsupported_type',
    generic: 'drop.error.generic',
};

/** Per-file upload lifecycle. */
type FileStatus =
    | { kind: 'pending' }
    | { kind: 'sealing' }
    | { kind: 'uploading'; percent: number }
    | { kind: 'done' }
    | { kind: 'failed'; code: DropFailureCode };

interface SelectedFile {
    /** A stable per-selection id (the file list is never reordered; used as the React key). */
    id: string;
    file: File;
    status: FileStatus;
}

function DropUploader() {
    const { opaqueId } = Route.useParams();

    // The fragment carries the Drop Key (+ passphrase params) and is never sent to the server.
    const link = useMemo(
        () => parseDropFragment(window.location.hash.replace(/^#/, '')),
        [],
    );

    if (!link) {
        return (
            <Shell>
                <Notice
                    titleId="drop.incomplete.title"
                    bodyId="drop.incomplete.body"
                />
            </Shell>
        );
    }
    return <DropForm opaqueId={opaqueId} link={link} />;
}

function DropForm({
    opaqueId,
    link,
}: {
    opaqueId: string;
    link: NonNullable<ReturnType<typeof parseDropFragment>>;
}) {
    const [files, setFiles] = useState<SelectedFile[]>([]);
    const [passphrase, setPassphrase] = useState('');
    const [passphraseWrong, setPassphraseWrong] = useState(false);
    const [busy, setBusy] = useState(false);
    const inputRef = useRef<HTMLInputElement>(null);

    const protectedLink = link.passphrase !== null;
    const allDone =
        files.length > 0 && files.every((f) => f.status.kind === 'done');
    const doneCount = files.filter((f) => f.status.kind === 'done').length;

    const setStatus = useCallback((index: number, status: FileStatus) => {
        setFiles((prev) =>
            prev.map((f, i) => (i === index ? { ...f, status } : f)),
        );
    }, []);

    const onPick = (e: React.ChangeEvent<HTMLInputElement>) => {
        const picked = Array.from(e.target.files ?? []);
        setFiles(
            picked.map((file, i) => ({
                id: `${Date.now()}-${i}-${file.name}`,
                file,
                status: { kind: 'pending' },
            })),
        );
        // Allow re-picking the same file(s) to reset the flow.
        if (inputRef.current) inputRef.current.value = '';
    };

    /** Seal + upload one file, threading its lifecycle into per-file state. */
    const uploadOne = useCallback(
        async (
            index: number,
            file: File,
            passphraseProof: string | null,
        ): Promise<'ok' | DropFailureCode> => {
            try {
                setStatus(index, { kind: 'sealing' });
                const bytes = new Uint8Array(await file.arrayBuffer());
                const contentType = file.type || 'application/octet-stream';
                const sealed = sealAsset(
                    bytes,
                    link.dropPubkey,
                    contentType,
                    file.name || null,
                );
                setStatus(index, { kind: 'uploading', percent: 0 });
                await uploadDrop({
                    base: DROP_BASE,
                    opaqueId,
                    sealed,
                    passphraseProof,
                    onProgress: (fraction) =>
                        setStatus(index, {
                            kind: 'uploading',
                            percent: Math.round(fraction * 100),
                        }),
                });
                setStatus(index, { kind: 'done' });
                return 'ok';
            } catch (err) {
                const code =
                    err instanceof DropUploadError ? err.code : 'generic';
                setStatus(index, { kind: 'failed', code });
                return code;
            }
        },
        [link.dropPubkey, opaqueId, setStatus],
    );

    /** Upload every not-yet-done file sequentially. */
    const uploadAll = useCallback(async () => {
        if (protectedLink && !passphrase) return;
        setBusy(true);
        setPassphraseWrong(false);

        // Derive the passphrase possession proof once, client-side; the passphrase never leaves.
        let proof: string | null = null;
        if (link.passphrase) {
            try {
                await initDropWasm();
                proof = dropPassphraseProof(
                    passphrase,
                    link.passphrase.saltHex,
                    link.passphrase.memKib,
                    link.passphrase.tCost,
                    link.passphrase.pCost,
                );
            } catch {
                setBusy(false);
                return;
            }
        } else {
            await initDropWasm();
        }

        const targets = files
            .map((f, i) => ({ f, i }))
            .filter(({ f }) => f.status.kind !== 'done');
        for (const { f, i } of targets) {
            // Sequential by design: one drop at a time keeps memory bounded and the owner's
            // per-link caps + quota checks strictly ordered.
            const outcome = await uploadOne(i, f.file, proof);
            if (outcome === 'passphrase') {
                // A rejected proof: prompt again and stop (the rest would fail identically).
                setPassphraseWrong(true);
                break;
            }
        }
        setBusy(false);
    }, [files, link.passphrase, passphrase, protectedLink, uploadOne]);

    if (allDone) {
        return (
            <Shell>
                <Card className="mx-auto w-full max-w-md">
                    <CardHeader>
                        <CardTitle className="flex items-center gap-2">
                            <CheckCircle2Icon className="h-5 w-5 text-green-600" />
                            <FormattedMessage id="drop.done.title" />
                        </CardTitle>
                        <CardDescription>
                            <FormattedMessage id="drop.done.body" />
                        </CardDescription>
                    </CardHeader>
                </Card>
            </Shell>
        );
    }

    return (
        <Shell>
            <Card className="mx-auto w-full max-w-xl">
                <CardHeader>
                    <div className="flex items-center justify-between gap-2">
                        <CardTitle>
                            <FormattedMessage id="drop.title" />
                        </CardTitle>
                        <span className="inline-flex items-center gap-1 rounded-full bg-muted px-3 py-1 text-xs text-muted-foreground">
                            <UploadIcon className="h-3.5 w-3.5" />
                            <FormattedMessage id="drop.upload_only_badge" />
                        </span>
                    </div>
                    <CardDescription>
                        <FormattedMessage id="drop.subtitle" />
                    </CardDescription>
                </CardHeader>
                <CardContent className="grid gap-4">
                    {protectedLink && (
                        <div className="grid gap-2">
                            <Label
                                htmlFor="drop-passphrase"
                                className="flex items-center gap-2"
                            >
                                <LockIcon className="h-4 w-4" />
                                <FormattedMessage id="drop.passphrase.label" />
                            </Label>
                            <Input
                                id="drop-passphrase"
                                type="password"
                                autoComplete="off"
                                value={passphrase}
                                disabled={busy}
                                onChange={(e) => setPassphrase(e.target.value)}
                            />
                            {passphraseWrong && (
                                <p className="text-sm text-destructive">
                                    <FormattedMessage id="drop.passphrase.wrong" />
                                </p>
                            )}
                        </div>
                    )}

                    <div className="grid gap-2">
                        <Label htmlFor="drop-files">
                            <FormattedMessage id="drop.picker.label" />
                        </Label>
                        <input
                            ref={inputRef}
                            id="drop-files"
                            type="file"
                            multiple
                            className="hidden"
                            onChange={onPick}
                        />
                        <Button
                            type="button"
                            variant="outline"
                            disabled={busy}
                            onClick={() => inputRef.current?.click()}
                        >
                            <FormattedMessage id="drop.picker.button" />
                        </Button>
                    </div>

                    {files.length === 0 ? (
                        <p className="text-sm text-muted-foreground">
                            <FormattedMessage id="drop.picker.empty" />
                        </p>
                    ) : (
                        <ul className="grid gap-2">
                            {files.map((f, i) => (
                                <FileRow
                                    key={f.id}
                                    file={f}
                                    onRetry={() =>
                                        void uploadOne(
                                            i,
                                            f.file,
                                            /* proof handled in uploadAll; single retry re-derives via uploadAll */ null,
                                        )
                                    }
                                    protectedLink={protectedLink}
                                />
                            ))}
                        </ul>
                    )}

                    {files.length > 0 && (
                        <div className="flex items-center justify-between gap-3">
                            <span className="text-sm text-muted-foreground">
                                <FormattedMessage
                                    id="drop.summary"
                                    values={{
                                        done: doneCount,
                                        total: files.length,
                                    }}
                                />
                            </span>
                            <Button
                                type="button"
                                disabled={
                                    busy ||
                                    (protectedLink && !passphrase) ||
                                    allDone
                                }
                                onClick={() => void uploadAll()}
                            >
                                {busy ? (
                                    <FormattedMessage id="drop.upload.busy" />
                                ) : (
                                    <FormattedMessage
                                        id="drop.upload.button"
                                        values={{ count: files.length }}
                                    />
                                )}
                            </Button>
                        </div>
                    )}

                    <p className="flex items-start gap-2 rounded-md bg-muted/50 p-3 text-xs text-muted-foreground">
                        <ShieldCheckIcon className="mt-0.5 h-4 w-4 flex-shrink-0" />
                        <FormattedMessage id="drop.privacy_note" />
                    </p>
                </CardContent>
            </Card>
        </Shell>
    );
}

/** One file's row: name + a localized status (and a retry button when it failed). Retrying a
 *  passphrase-gated link requires the passphrase, so the retry falls back to a whole-batch run. */
function FileRow({
    file,
    onRetry,
    protectedLink,
}: {
    file: SelectedFile;
    onRetry: () => void;
    protectedLink: boolean;
}) {
    const intl = useIntl();
    const { status } = file;
    let statusText: string;
    switch (status.kind) {
        case 'pending':
            statusText = intl.formatMessage({ id: 'drop.file.pending' });
            break;
        case 'sealing':
            statusText = intl.formatMessage({ id: 'drop.file.sealing' });
            break;
        case 'uploading':
            statusText = intl.formatMessage(
                { id: 'drop.file.uploading' },
                { percent: status.percent },
            );
            break;
        case 'done':
            statusText = intl.formatMessage({ id: 'drop.file.done' });
            break;
        case 'failed':
            statusText = intl.formatMessage({ id: FAILURE_KEY[status.code] });
            break;
    }

    return (
        <li className="flex items-center justify-between gap-3 rounded-md border p-2 text-sm">
            <span className="min-w-0 flex-1 truncate font-mono">
                {file.file.name}
            </span>
            <span
                className={
                    status.kind === 'failed'
                        ? 'text-destructive'
                        : status.kind === 'done'
                          ? 'text-green-600'
                          : 'text-muted-foreground'
                }
            >
                {statusText}
            </span>
            {status.kind === 'failed' && !protectedLink && (
                <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={onRetry}
                >
                    <FormattedMessage id="drop.file.retry" />
                </Button>
            )}
        </li>
    );
}

/** Full-page, centered container for the unauthenticated guest surface. */
function Shell({ children }: { children: React.ReactNode }) {
    return (
        <div className="flex min-h-screen flex-col items-center justify-center bg-muted/40 p-4">
            <div className="w-full max-w-2xl">{children}</div>
        </div>
    );
}

/** A titled failure/notice card. */
function Notice({ titleId, bodyId }: { titleId: string; bodyId: string }) {
    return (
        <Card className="mx-auto w-full max-w-sm">
            <CardHeader>
                <CardTitle>
                    <FormattedMessage id={titleId} />
                </CardTitle>
                <CardDescription>
                    <FormattedMessage id={bodyId} />
                </CardDescription>
            </CardHeader>
        </Card>
    );
}
