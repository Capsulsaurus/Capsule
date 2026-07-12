// Guest share-link viewer (slice S-E1): `https://server.tld/s/{opaque-id}#{secret}`.
//
// No auth, no account. The `#{secret}` fragment stays in the browser (it is never sent to the
// server); a passphrase, when the link is protected, is unwrapped entirely client-side via the
// `capsule-wasm` module. A not-found / revoked / expired link is one indistinguishable server
// `404`, surfaced here as a single generic "unavailable" message (SSoT: the Share Links design
// doc — scenario #33). This route renders a read-only view; a share link never grants write
// access.

import { createFileRoute } from '@tanstack/react-router';
import { filesize } from 'filesize';
import { EyeIcon, LockIcon } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useMemo, useState } from 'react';
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
import { initShareWasm, openShare, shareOpenCode } from '@/lib/share-open';

export const Route = createFileRoute('/s/$opaqueId')({
    component: ShareViewer,
});

/** Base origin for the share serve endpoints. Empty (same-origin) in production — the home
 *  server serves both this viewer and `/s/…`; overridable for split dev deployments. */
const SHARE_BASE = import.meta.env.PUBLIC_SHARE_URL ?? '';

/** One covered asset's served (already privacy-stripped) metadata. */
interface ShareAsset {
    asset_id: string;
    content_hash: string;
    content_type: string;
    size: number;
    metadata_blob: string;
}

/** The `GET /s/{opaque-id}` response body. */
interface ShareMetadata {
    scope: 'album' | 'asset';
    scope_id: string;
    home_server: string;
    passphrase_protected: boolean;
    expires_at: string | null;
    assets: ShareAsset[];
}

/** The finite view states of the guest viewer. */
type View =
    | { kind: 'loading' }
    | { kind: 'incomplete' }
    | { kind: 'unavailable' }
    | { kind: 'error' }
    | {
          kind: 'passphrase';
          meta: ShareMetadata;
          wrapped: string;
          fragment: string;
          wrong: boolean;
          busy: boolean;
      }
    | { kind: 'ready'; meta: ShareMetadata };

function ShareViewer() {
    const { opaqueId } = Route.useParams();
    const [view, setView] = useState<View>({ kind: 'loading' });

    // The URL fragment carries the link secret and is never sent to the server. Read it once.
    const fragment = useMemo(
        () => window.location.hash.replace(/^#/, '').trim(),
        [],
    );

    /** Attempt to open the scope client-side; move to `ready` or report a wrong secret. */
    const attemptOpen = useCallback(
        async (
            meta: ShareMetadata,
            wrapped: string,
            passphrase: string | null,
        ): Promise<'ok' | 'wrong' | 'error'> => {
            try {
                await initShareWasm();
                // Opening validates the fragment secret (and passphrase); we render the served,
                // already-stripped metadata read-only. Full-resolution blob decryption
                // (ShareScope.decryptBlob) activates once the serve response carries each asset's
                // crypto-manifest params (nonce prefix / AMK epoch) — a follow-up.
                const scope = openShare(
                    wrapped,
                    opaqueId,
                    fragment,
                    passphrase,
                );
                scope.free();
                setView({ kind: 'ready', meta });
                return 'ok';
            } catch (err) {
                const code = shareOpenCode(err);
                if (code === 'wrong_secret' || code === 'passphrase_required') {
                    return 'wrong';
                }
                return 'error';
            }
        },
        [opaqueId, fragment],
    );

    useEffect(() => {
        let cancelled = false;
        async function load() {
            if (!fragment) {
                setView({ kind: 'incomplete' });
                return;
            }
            try {
                const metaRes = await fetch(`${SHARE_BASE}/s/${opaqueId}`);
                // A peer that does not host the share returns a home-server pointer (never
                // content, never a redirect). Resolve it by reloading against the home server,
                // carrying the same fragment.
                if (metaRes.status === 421) {
                    const { home_server } = (await metaRes.json()) as {
                        home_server: string;
                    };
                    window.location.href = `https://${home_server}/s/${opaqueId}${window.location.hash}`;
                    return;
                }
                if (!metaRes.ok) {
                    setView({ kind: 'unavailable' });
                    return;
                }
                const meta = (await metaRes.json()) as ShareMetadata;

                const wrappedRes = await fetch(
                    `${SHARE_BASE}/s/${opaqueId}/wrapped-secret`,
                );
                if (!wrappedRes.ok) {
                    setView({ kind: 'unavailable' });
                    return;
                }
                const { wrapped_scope } = (await wrappedRes.json()) as {
                    wrapped_scope: string;
                };
                if (cancelled) return;

                if (meta.passphrase_protected) {
                    setView({
                        kind: 'passphrase',
                        meta,
                        wrapped: wrapped_scope,
                        fragment,
                        wrong: false,
                        busy: false,
                    });
                    return;
                }
                const outcome = await attemptOpen(meta, wrapped_scope, null);
                if (cancelled) return;
                if (outcome === 'wrong') {
                    // A wrong fragment on an unprotected link is indistinguishable from a
                    // missing link — one generic message.
                    setView({ kind: 'unavailable' });
                } else if (outcome === 'error') {
                    setView({ kind: 'error' });
                }
            } catch {
                if (!cancelled) setView({ kind: 'error' });
            }
        }
        void load();
        return () => {
            cancelled = true;
        };
    }, [opaqueId, fragment, attemptOpen]);

    return (
        <div className="flex min-h-screen flex-col items-center justify-center bg-muted/40 p-4">
            <div className="w-full max-w-2xl">
                {view.kind === 'loading' && (
                    <Centered messageId="share.viewer.opening" />
                )}
                {view.kind === 'incomplete' && (
                    <Notice
                        titleId="share.incomplete.title"
                        bodyId="share.incomplete.body"
                    />
                )}
                {view.kind === 'unavailable' && (
                    <Notice
                        titleId="share.unavailable.title"
                        bodyId="share.unavailable.body"
                    />
                )}
                {view.kind === 'error' && (
                    <Notice
                        titleId="share.error.title"
                        bodyId="share.error.body"
                    />
                )}
                {view.kind === 'passphrase' && (
                    <PassphraseGate
                        view={view}
                        setView={setView}
                        onOpen={attemptOpen}
                    />
                )}
                {view.kind === 'ready' && <ShareContents meta={view.meta} />}
            </div>
        </div>
    );
}

/** A centered single-line status (the loading state). */
function Centered({ messageId }: { messageId: string }) {
    return (
        <p className="text-center text-sm text-muted-foreground">
            <FormattedMessage id={messageId} />
        </p>
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

/** The client-side passphrase prompt: the passphrase is unwrapped in the browser and never sent
 *  to the server (scenario #42). */
function PassphraseGate({
    view,
    setView,
    onOpen,
}: {
    view: Extract<View, { kind: 'passphrase' }>;
    setView: (v: View) => void;
    onOpen: (
        meta: ShareMetadata,
        wrapped: string,
        passphrase: string | null,
    ) => Promise<'ok' | 'wrong' | 'error'>;
}) {
    const [passphrase, setPassphrase] = useState('');

    async function submit(e: React.FormEvent) {
        e.preventDefault();
        setView({ ...view, busy: true, wrong: false });
        const outcome = await onOpen(view.meta, view.wrapped, passphrase);
        if (outcome === 'ok') return; // onOpen advances to `ready`.
        if (outcome === 'error') {
            setView({ kind: 'error' });
            return;
        }
        setView({ ...view, busy: false, wrong: true });
    }

    return (
        <Card className="mx-auto w-full max-w-sm">
            <CardHeader>
                <CardTitle className="flex items-center gap-2">
                    <LockIcon className="h-5 w-5" />
                    <FormattedMessage id="share.passphrase.title" />
                </CardTitle>
                <CardDescription>
                    <FormattedMessage id="share.passphrase.body" />
                </CardDescription>
            </CardHeader>
            <CardContent>
                <form className="grid gap-4" onSubmit={submit}>
                    <div className="grid gap-2">
                        <Label htmlFor="share-passphrase">
                            <FormattedMessage id="share.passphrase.label" />
                        </Label>
                        <Input
                            id="share-passphrase"
                            type="password"
                            autoFocus
                            required
                            value={passphrase}
                            disabled={view.busy}
                            onChange={(e) => setPassphrase(e.target.value)}
                        />
                    </div>
                    {view.wrong && (
                        <p className="text-sm text-destructive">
                            <FormattedMessage id="share.passphrase.wrong" />
                        </p>
                    )}
                    <Button type="submit" disabled={view.busy || !passphrase}>
                        <FormattedMessage id="share.passphrase.submit" />
                    </Button>
                </form>
            </CardContent>
        </Card>
    );
}

/** The read-only contents view once the scope has been opened client-side. */
function ShareContents({ meta }: { meta: ShareMetadata }) {
    const intl = useIntl();
    return (
        <Card>
            <CardHeader>
                <div className="flex items-center justify-between gap-2">
                    <CardTitle>
                        <FormattedMessage
                            id={
                                meta.scope === 'album'
                                    ? 'share.scope.album'
                                    : 'share.scope.asset'
                            }
                        />
                    </CardTitle>
                    <span className="inline-flex items-center gap-1 rounded-full bg-muted px-3 py-1 text-xs text-muted-foreground">
                        <EyeIcon className="h-3.5 w-3.5" />
                        <FormattedMessage id="share.viewer.readonly_badge" />
                    </span>
                </div>
                <CardDescription>
                    <FormattedMessage
                        id="share.items"
                        values={{ count: meta.assets.length }}
                    />
                    {meta.expires_at && (
                        <>
                            {' · '}
                            <FormattedMessage
                                id="share.expires"
                                values={{
                                    date: intl.formatDate(meta.expires_at, {
                                        dateStyle: 'medium',
                                        timeStyle: 'short',
                                    }),
                                }}
                            />
                        </>
                    )}
                </CardDescription>
            </CardHeader>
            <CardContent className="grid gap-3">
                {meta.assets.map((asset) => (
                    <div
                        key={asset.asset_id}
                        className="grid gap-1 rounded-md border p-3 text-sm"
                    >
                        <div className="flex justify-between gap-2">
                            <span className="text-muted-foreground">
                                <FormattedMessage id="share.asset.type" />
                            </span>
                            <span className="font-mono">
                                {asset.content_type}
                            </span>
                        </div>
                        <div className="flex justify-between gap-2">
                            <span className="text-muted-foreground">
                                <FormattedMessage id="share.asset.size" />
                            </span>
                            <span className="font-mono">
                                {filesize(asset.size)}
                            </span>
                        </div>
                    </div>
                ))}
            </CardContent>
        </Card>
    );
}
