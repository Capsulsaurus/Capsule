// Where the identity provider sends the person back: `/login/oidc-callback?state=…&code=…`.
//
// Under `/login` so the auth gate treats it as public, and flat (`login_`) so it does not render
// inside the login card. It takes the binding secret this tab stored when the ceremony began,
// posts `state`, `code` and that secret to `/v1/auth/oidc/callback`, and ends exactly where a
// password sign-in ends: a session, or the authenticator step. It writes no library state.

import { createLazyFileRoute, Link, useNavigate } from '@tanstack/react-router';
import { useEffect, useRef, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { signInErrorText, TotpSignInCard } from '@/components/mfa/totp-sign-in';
import { Button } from '@/components/ui/button';
import {
    Card,
    CardContent,
    CardFooter,
    CardHeader,
    CardTitle,
} from '@/components/ui/card';
import { completeOidcLogin, needsSecondFactor } from '@/lib/api';
import type { TokenPair } from '@/lib/auth';
import { useAuth } from '@/lib/auth-context';
import {
    CEREMONY_KEY,
    OIDC_CALLBACK_PATH,
    readCallback,
    takeCeremony,
} from '@/lib/oidc';

export const Route = createLazyFileRoute('/login_/oidc-callback')({
    component: OidcCallback,
});

type CallbackStep =
    | { kind: 'completing' }
    | { kind: 'totp'; mfaToken: string }
    | { kind: 'failed'; message: string };

function OidcCallback() {
    const intl = useIntl();
    const { setTokens } = useAuth();
    const navigate = useNavigate();
    const [step, setStep] = useState<CallbackStep>({ kind: 'completing' });
    // The ceremony is single-use, so the callback must run once even where an effect runs twice
    // (React's development double-invoke).
    const started = useRef(false);

    useEffect(() => {
        if (started.current) return;
        started.current = true;

        function signedIn(tokens: TokenPair) {
            setTokens(tokens);
            navigate({ to: '/photos', replace: true });
        }

        const params = readCallback(window.location.search);
        // The code and state are spent from here on; keep them out of the history entry.
        window.history.replaceState(null, '', OIDC_CALLBACK_PATH);

        if (params.kind !== 'code') {
            sessionStorage.removeItem(CEREMONY_KEY);
            setStep({
                kind: 'failed',
                message: intl.formatMessage({ id: 'auth.oidc.not_completed' }),
            });
            return;
        }
        const verifier = takeCeremony(sessionStorage, params.state);
        if (!verifier) {
            setStep({
                kind: 'failed',
                message: intl.formatMessage({ id: 'auth.oidc.ceremony_lost' }),
            });
            return;
        }
        completeOidcLogin({
            state: params.state,
            code: params.code,
            binding_verifier: verifier,
        })
            .then((result) => {
                if (needsSecondFactor(result)) {
                    setStep({ kind: 'totp', mfaToken: result.mfa_token });
                } else {
                    signedIn(result);
                }
            })
            .catch((err) => {
                setStep({
                    kind: 'failed',
                    message: signInErrorText(intl, err),
                });
            });
    }, [intl, navigate, setTokens]);

    return (
        <div className="flex flex-col items-center justify-center min-h-screen bg-muted/40 p-4">
            {step.kind === 'totp' ? (
                <TotpSignInCard
                    mfaToken={step.mfaToken}
                    onSignedIn={(tokens) => {
                        setTokens(tokens);
                        navigate({ to: '/photos', replace: true });
                    }}
                    onBack={() => navigate({ to: '/login', replace: true })}
                />
            ) : (
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">
                            {step.kind === 'completing' ? (
                                <FormattedMessage id="auth.oidc.completing" />
                            ) : (
                                <FormattedMessage id="auth.oidc.failed_title" />
                            )}
                        </CardTitle>
                    </CardHeader>
                    {step.kind === 'failed' && (
                        <>
                            <CardContent>
                                <p className="text-sm text-destructive">
                                    {step.message}
                                </p>
                            </CardContent>
                            <CardFooter>
                                <Button asChild className="w-full">
                                    <Link to="/login" replace>
                                        <FormattedMessage id="auth.back_to_login" />
                                    </Link>
                                </Button>
                            </CardFooter>
                        </>
                    )}
                </Card>
            )}
        </div>
    );
}
