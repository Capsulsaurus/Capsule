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
import { authenticateWithPasskey } from '@/lib/webauthn';
import { Link, createLazyFileRoute, useNavigate } from '@tanstack/react-router';
import { KeyRoundIcon, MountainIcon } from 'lucide-react';
import type React from 'react';
import { useEffect, useState } from 'react';

export const Route = createLazyFileRoute('/login')({
    component: Login,
});

type LoginStep = 'credentials' | 'totp';

function Login() {
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
                    : 'An unexpected error occurred.',
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
                    : 'An unexpected error occurred.',
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
                setError('Passkey authentication was cancelled.');
            } else {
                setError('Passkey authentication failed.');
            }
        } finally {
            setLoading(false);
        }
    }

    return (
        <div className="flex flex-col items-center justify-center min-h-screen bg-muted/40 p-4">
            <Link to="/" className="mb-8 flex items-center gap-2">
                <MountainIcon className="h-8 w-8 text-primary" />
                <span className="text-2xl font-bold text-primary">Capsule</span>
            </Link>

            {step === 'credentials' ? (
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">Login</CardTitle>
                        <CardDescription>
                            Enter your email below to login to your account.
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
                                <Label htmlFor="email">Email</Label>
                                <Input
                                    id="email"
                                    type="email"
                                    placeholder="m@example.com"
                                    required
                                    value={email}
                                    onChange={(e) => setEmail(e.target.value)}
                                    disabled={loading}
                                />
                            </div>
                            <div className="grid gap-2">
                                <Label htmlFor="password">Password</Label>
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
                                {loading ? 'Signing in…' : 'Sign in'}
                            </Button>
                            <div className="relative w-full">
                                <div className="absolute inset-0 flex items-center">
                                    <span className="w-full border-t" />
                                </div>
                                <div className="relative flex justify-center text-xs uppercase">
                                    <span className="bg-card px-2 text-muted-foreground">
                                        or
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
                                Sign in with Passkey
                            </Button>
                            <p className="text-xs text-muted-foreground text-center">
                                Don't have an account?{' '}
                                <Link to="/register" className="underline">
                                    Sign up
                                </Link>
                            </p>
                            <p className="text-xs text-muted-foreground text-center">
                                <Link
                                    to="/forgot-password"
                                    className="underline"
                                >
                                    Forgot password?
                                </Link>
                            </p>
                        </CardFooter>
                    </form>
                </Card>
            ) : (
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">
                            Two-Factor Auth
                        </CardTitle>
                        <CardDescription>
                            Enter the 6-digit code from your authenticator app.
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
                                <Label htmlFor="totp">Authenticator Code</Label>
                                <Input
                                    id="totp"
                                    type="text"
                                    inputMode="numeric"
                                    placeholder="123456"
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
                                {loading ? 'Verifying…' : 'Verify'}
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
                                Back
                            </Button>
                        </CardFooter>
                    </form>
                </Card>
            )}
        </div>
    );
}
