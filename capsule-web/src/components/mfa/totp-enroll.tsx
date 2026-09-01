/**
 * TOTP enrollment flow:
 * 1. Call POST /auth/totp/enroll to get provisioning_uri
 * 2. Show QR code and provisioning URI
 * 3. User scans with their authenticator app
 * 4. User enters a code to confirm enrollment
 * 5. Call POST /auth/totp/verify-enrollment with the code
 */

import { useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import QRCode from 'react-qr-code';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ApiError, totpEnroll, totpVerifyEnrollment } from '@/lib/api';

interface TotpEnrollProps {
    onSuccess: () => void;
    onCancel: () => void;
}

type EnrollStep = 'start' | 'scan' | 'verify';

export function TotpEnroll({ onSuccess, onCancel }: TotpEnrollProps) {
    const intl = useIntl();
    const [step, setStep] = useState<EnrollStep>('start');
    const [provisioningUri, setProvisioningUri] = useState('');
    const [code, setCode] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    async function handleStart() {
        setError(null);
        setLoading(true);
        try {
            const { provisioning_uri } = await totpEnroll();
            setProvisioningUri(provisioning_uri);
            setStep('scan');
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({ id: 'mfa.totp.start_failed' }),
            );
        } finally {
            setLoading(false);
        }
    }

    async function handleVerify(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            await totpVerifyEnrollment(code);
            onSuccess();
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({ id: 'mfa.totp.invalid_code' }),
            );
        } finally {
            setLoading(false);
        }
    }

    if (step === 'start') {
        return (
            <div className="space-y-4">
                <p className="text-sm text-muted-foreground">
                    <FormattedMessage id="mfa.totp.start_description" />
                </p>
                <div className="flex gap-2">
                    <Button onClick={handleStart} disabled={loading}>
                        {loading ? (
                            <FormattedMessage id="mfa.totp.starting" />
                        ) : (
                            <FormattedMessage id="mfa.totp.setup" />
                        )}
                    </Button>
                    <Button variant="ghost" onClick={onCancel}>
                        <FormattedMessage id="common.cancel" />
                    </Button>
                </div>
            </div>
        );
    }

    if (step === 'scan') {
        return (
            <div className="space-y-4">
                <p className="text-sm text-muted-foreground">
                    <FormattedMessage id="mfa.totp.scan_description" />
                </p>
                <div className="flex justify-center p-4 bg-white rounded-md">
                    <QRCode value={provisioningUri} size={180} />
                </div>
                <details className="text-xs text-muted-foreground">
                    <summary className="cursor-pointer select-none">
                        <FormattedMessage id="mfa.totp.show_key" />
                    </summary>
                    <p className="mt-1 break-all font-mono">
                        {provisioningUri}
                    </p>
                </details>
                <Button onClick={() => setStep('verify')} className="w-full">
                    <FormattedMessage id="mfa.totp.scanned" />
                </Button>
            </div>
        );
    }

    return (
        <form onSubmit={handleVerify} className="space-y-4">
            <p className="text-sm text-muted-foreground">
                <FormattedMessage id="mfa.totp.verify_description" />
            </p>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <div className="grid gap-2">
                <Label htmlFor="totp-verify">
                    <FormattedMessage id="mfa.totp.code_label" />
                </Label>
                <Input
                    id="totp-verify"
                    type="text"
                    inputMode="numeric"
                    placeholder={intl.formatMessage({
                        id: 'common.code_placeholder',
                    })}
                    maxLength={6}
                    required
                    value={code}
                    onChange={(e) => setCode(e.target.value)}
                    disabled={loading}
                    autoFocus
                />
            </div>
            <div className="flex gap-2">
                <Button type="submit" disabled={loading}>
                    {loading ? (
                        <FormattedMessage id="auth.verifying" />
                    ) : (
                        <FormattedMessage id="mfa.totp.confirm" />
                    )}
                </Button>
                <Button
                    variant="ghost"
                    type="button"
                    onClick={() => setStep('scan')}
                >
                    <FormattedMessage id="common.back" />
                </Button>
            </div>
        </form>
    );
}
