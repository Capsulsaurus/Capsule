/**
 * The second step of a sign-in on an account with a confirmed authenticator: the code that turns
 * a `202` challenge into a session. Shared by both doors that can answer one — the password login
 * and the identity-provider callback — so the two cannot drift.
 */

import type React from 'react';
import { useState } from 'react';
import { FormattedMessage, type IntlShape, useIntl } from 'react-intl';
import { Button } from '@/components/ui/button';
import {
    Card,
    CardContent,
    CardDescription,
    CardFooter,
    CardHeader,
    CardTitle,
} from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ApiError, verifyTotpLogin } from '@/lib/api';
import type { TokenPair } from '@/lib/auth';

/**
 * What to show for a failed sign-in request: the localized message for the server's stable
 * `error.*` code when the catalog has one, else the server's English detail, else the generic
 * fallback.
 */
export function signInErrorText(intl: IntlShape, err: unknown): string {
    if (err instanceof ApiError) {
        if (err.code && err.code in intl.messages) {
            return intl.formatMessage({ id: err.code });
        }
        return err.message;
    }
    return intl.formatMessage({ id: 'auth.error.unexpected' });
}

interface TotpSignInCardProps {
    /** The challenge's `mfa_token`. */
    mfaToken: string;
    /** The session the code opened. */
    onSignedIn: (tokens: TokenPair) => void;
    /** Leave the step; the challenge is abandoned. */
    onBack: () => void;
}

export function TotpSignInCard({
    mfaToken,
    onSignedIn,
    onBack,
}: TotpSignInCardProps) {
    const intl = useIntl();
    const [totpCode, setTotpCode] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    async function handleSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            onSignedIn(await verifyTotpLogin(mfaToken, totpCode));
        } catch (err) {
            setError(signInErrorText(intl, err));
        } finally {
            setLoading(false);
        }
    }

    return (
        <Card className="w-full max-w-sm">
            <CardHeader>
                <CardTitle className="text-2xl">
                    <FormattedMessage id="auth.totp.title" />
                </CardTitle>
                <CardDescription>
                    <FormattedMessage id="auth.totp.description" />
                </CardDescription>
            </CardHeader>
            <form onSubmit={handleSubmit}>
                <CardContent className="grid gap-4">
                    {error && (
                        <p className="text-sm text-destructive">{error}</p>
                    )}
                    <div className="grid gap-2">
                        <Label htmlFor="totp">
                            <FormattedMessage id="auth.totp.code_label" />
                        </Label>
                        <Input
                            id="totp"
                            type="text"
                            inputMode="numeric"
                            placeholder={intl.formatMessage({
                                id: 'common.code_placeholder',
                            })}
                            maxLength={6}
                            required
                            value={totpCode}
                            onChange={(e) => setTotpCode(e.target.value)}
                            disabled={loading}
                            autoFocus
                        />
                    </div>
                </CardContent>
                <CardFooter className="flex flex-col gap-3">
                    <Button className="w-full" type="submit" disabled={loading}>
                        {loading ? (
                            <FormattedMessage id="auth.verifying" />
                        ) : (
                            <FormattedMessage id="auth.verify" />
                        )}
                    </Button>
                    <Button
                        variant="ghost"
                        className="w-full"
                        type="button"
                        onClick={onBack}
                    >
                        <FormattedMessage id="common.back" />
                    </Button>
                </CardFooter>
            </form>
        </Card>
    );
}
