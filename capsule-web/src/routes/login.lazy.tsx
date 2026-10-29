import { useQuery } from '@tanstack/react-query';
import { createLazyFileRoute, Link, useNavigate } from '@tanstack/react-router';
import { MountainIcon } from 'lucide-react';
import type React from 'react';
import { useEffect, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { signInErrorText, TotpSignInCard } from '@/components/mfa/totp-sign-in';
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
    beginOidcLogin,
    login,
    needsSecondFactor,
    oidcAvailable,
} from '@/lib/api';
import type { TokenPair } from '@/lib/auth';
import { useAuth } from '@/lib/auth-context';
import { APP_NAME } from '@/lib/constant';
import {
    bindingChallenge,
    callbackUri,
    freshBindingVerifier,
    saveCeremony,
} from '@/lib/oidc';

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
    const [mfaToken, setMfaToken] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    // `server-info` publishes `auth.oidc: null` on a deployment without an identity provider, so
    // the option is offered only where it can work. A failed read hides it rather than blocking
    // the password form.
    const { data: offersOidc } = useQuery({
        queryKey: ['server-info', 'oidc'],
        queryFn: oidcAvailable,
        retry: false,
        staleTime: 5 * 60 * 1000,
    });

    // Redirect already-authenticated users away from login
    useEffect(() => {
        if (!isLoading && isAuthenticated) {
            navigate({ to: '/photos', replace: true });
        }
    }, [isLoading, isAuthenticated, navigate]);

    function signedIn(tokens: TokenPair) {
        setTokens(tokens);
        navigate({ to: '/photos' });
    }

    async function handleCredentialsSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            const result = await login({ email, password });
            // `202 Accepted` — the password verified and the sign-in is not finished. No session
            // exists yet, which is why there is nothing to store here.
            if (needsSecondFactor(result)) {
                setMfaToken(result.mfa_token);
                setStep('totp');
            } else {
                signedIn(result);
            }
        } catch (err) {
            setError(signInErrorText(intl, err));
        } finally {
            setLoading(false);
        }
    }

    /**
     * Begin the identity-provider ceremony and leave for the provider. The binding secret stays
     * in this tab's `sessionStorage` until `/login/oidc-callback` takes it; only its hash goes to
     * the server.
     */
    async function handleOidc() {
        setError(null);
        setLoading(true);
        try {
            const verifier = freshBindingVerifier();
            const ceremony = await beginOidcLogin({
                redirect_uri: callbackUri(window.location.origin),
                binding_challenge: await bindingChallenge(verifier),
            });
            saveCeremony(sessionStorage, {
                state: ceremony.state,
                verifier,
                expiresBy: ceremony.expires_by,
            });
            window.location.assign(ceremony.authorization_url);
        } catch (err) {
            setError(signInErrorText(intl, err));
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
                            {offersOidc && (
                                <>
                                    <p className="text-xs text-muted-foreground text-center">
                                        <FormattedMessage id="auth.login.or" />
                                    </p>
                                    <Button
                                        className="w-full"
                                        variant="outline"
                                        type="button"
                                        onClick={handleOidc}
                                        disabled={loading}
                                    >
                                        <FormattedMessage id="app.auth.login.oidc" />
                                    </Button>
                                </>
                            )}
                            {/* No passkey button: passkeys are not in v1, and the
                                six operations behind this one could never authenticate. No
                                "forgot password" link either: on an end-to-end-encrypted
                                account there is no reset, because the server cannot re-wrap a
                                master key it has never seen. Recovery is the escrow
                                blob, and it needs a screen of its own rather than a link that
                                promises the wrong thing. */}
                            <p className="text-xs text-muted-foreground text-center">
                                <FormattedMessage id="auth.no_account" />{' '}
                                <Link to="/register" className="underline">
                                    <FormattedMessage id="auth.sign_up" />
                                </Link>
                            </p>
                        </CardFooter>
                    </form>
                </Card>
            ) : (
                <TotpSignInCard
                    mfaToken={mfaToken}
                    onSignedIn={signedIn}
                    onBack={() => {
                        setStep('credentials');
                        setError(null);
                    }}
                />
            )}
        </div>
    );
}
