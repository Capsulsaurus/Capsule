import { createLazyFileRoute, Link, useNavigate } from '@tanstack/react-router';
import { KeyRoundIcon, MountainIcon } from 'lucide-react';
import type React from 'react';
import { useEffect, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
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
import {
    ApiError,
    login,
    passkeyLoginFinish,
    passkeyLoginStart,
    verifyTotpLogin,
} from '@/lib/api';
import { useAuth } from '@/lib/auth-context';
import { APP_NAME } from '@/lib/constant';
import { authenticateWithPasskey } from '@/lib/webauthn';

export const Route = createLazyFileRoute('/login')({
    component: Login,
});

type LoginStep = 'credentials' | 'totp';

function Login() {
    const intl = useIntl();
    const { setTokens, isAuthenticated, isLoading } = useAuth();
    const navigate = useNavigate();

    const [step, setStep] = useState<LoginStep>('credentials');
    const [email, setEmail] = useState('');
    const [password, setPassword] = useState('');
    const [totpCode, setTotpCode] = useState('');
    const [mfaToken, setMfaToken] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    // Redirect already-authenticated users away from login
    useEffect(() => {
        if (!isLoading && isAuthenticated) {
            navigate({ to: '/photos', replace: true });
        }
    }, [isLoading, isAuthenticated, navigate]);

    async function handleCredentialsSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            const result = await login({ email, password });
            if ('mfa_required' in result && result.mfa_required) {
                setMfaToken(result.mfa_token);
                setStep('totp');
            } else {
                setTokens(result);
                navigate({ to: '/photos' });
            }
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({ id: 'auth.error.unexpected' }),
            );
        } finally {
            setLoading(false);
        }
    }

    async function handleTotpSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            const tokens = await verifyTotpLogin(mfaToken, totpCode);
            setTokens(tokens);
            navigate({ to: '/photos' });
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({ id: 'auth.error.unexpected' }),
            );
        } finally {
            setLoading(false);
        }
    }

    async function handlePasskeyLogin() {
        setError(null);
        setLoading(true);
        try {
            const options = await passkeyLoginStart(email || undefined);
            const credential = await authenticateWithPasskey(options);
            const tokens = await passkeyLoginFinish(credential);
            setTokens(tokens);
            navigate({ to: '/photos' });
        } catch (err) {
            if (err instanceof ApiError) {
                setError(err.message);
            } else if (err instanceof Error && err.name === 'NotAllowedError') {
                setError(intl.formatMessage({ id: 'auth.passkey.cancelled' }));
            } else {
                setError(intl.formatMessage({ id: 'auth.passkey.failed' }));
            }
        } finally {
            setLoading(false);
        }
    }

    return (
        <div className="flex flex-col items-center justify-center min-h-screen bg-muted/40 p-4">
            <Link to="/" className="mb-8 flex items-center gap-2">
                <MountainIcon className="h-8 w-8 text-primary" />
                <span className="text-2xl font-bold text-primary">
                    {APP_NAME}
                </span>
            </Link>

            {step === 'credentials' ? (
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">
                            <FormattedMessage id="auth.login.title" />
                        </CardTitle>
                        <CardDescription>
                            <FormattedMessage id="auth.login.description" />
                        </CardDescription>
                    </CardHeader>
                    <form onSubmit={handleCredentialsSubmit}>
                        <CardContent className="grid gap-4">
                            {error && (
                                <p className="text-sm text-destructive">
                                    {error}
                                </p>
                            )}
                            <div className="grid gap-2">
                                <Label htmlFor="email">
                                    <FormattedMessage id="common.email" />
                                </Label>
                                <Input
                                    id="email"
                                    type="email"
                                    placeholder={intl.formatMessage({
                                        id: 'common.email_placeholder',
                                    })}
                                    required
                                    value={email}
                                    onChange={(e) => setEmail(e.target.value)}
                                    disabled={loading}
                                />
                            </div>
                            <div className="grid gap-2">
                                <Label htmlFor="password">
                                    <FormattedMessage id="common.password" />
                                </Label>
                                <Input
                                    id="password"
                                    type="password"
                                    required
                                    value={password}
                                    onChange={(e) =>
                                        setPassword(e.target.value)
                                    }
                                    disabled={loading}
                                />
                            </div>
                        </CardContent>
                        <CardFooter className="flex flex-col gap-3">
                            <Button
                                className="w-full"
                                type="submit"
                                disabled={loading}
                            >
                                {loading ? (
                                    <FormattedMessage id="auth.signing_in" />
                                ) : (
                                    <FormattedMessage id="common.sign_in" />
                                )}
                            </Button>
                            <div className="relative w-full">
                                <div className="absolute inset-0 flex items-center">
                                    <span className="w-full border-t" />
                                </div>
                                <div className="relative flex justify-center text-xs uppercase">
                                    <span className="bg-card px-2 text-muted-foreground">
                                        <FormattedMessage id="auth.or" />
                                    </span>
                                </div>
                            </div>
                            <Button
                                variant="outline"
                                className="w-full"
                                type="button"
                                disabled={loading}
                                onClick={handlePasskeyLogin}
                            >
                                <KeyRoundIcon className="mr-2 h-4 w-4" />
                                <FormattedMessage id="auth.sign_in_passkey" />
                            </Button>
                            <p className="text-xs text-muted-foreground text-center">
                                <FormattedMessage id="auth.no_account" />{' '}
                                <Link to="/register" className="underline">
                                    <FormattedMessage id="auth.sign_up" />
                                </Link>
                            </p>
                            <p className="text-xs text-muted-foreground text-center">
                                <Link
                                    to="/forgot-password"
                                    className="underline"
                                >
                                    <FormattedMessage id="auth.forgot_password_link" />
                                </Link>
                            </p>
                        </CardFooter>
                    </form>
                </Card>
            ) : (
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">
                            <FormattedMessage id="auth.totp.title" />
                        </CardTitle>
                        <CardDescription>
                            <FormattedMessage id="auth.totp.description" />
                        </CardDescription>
                    </CardHeader>
                    <form onSubmit={handleTotpSubmit}>
                        <CardContent className="grid gap-4">
                            {error && (
                                <p className="text-sm text-destructive">
                                    {error}
                                </p>
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
                                    onChange={(e) =>
                                        setTotpCode(e.target.value)
                                    }
                                    disabled={loading}
                                    autoFocus
                                />
                            </div>
                        </CardContent>
                        <CardFooter className="flex flex-col gap-3">
                            <Button
                                className="w-full"
                                type="submit"
                                disabled={loading}
                            >
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
                                onClick={() => {
                                    setStep('credentials');
                                    setError(null);
                                    setTotpCode('');
                                }}
                            >
                                <FormattedMessage id="common.back" />
                            </Button>
                        </CardFooter>
                    </form>
                </Card>
            )}
        </div>
    );
}
