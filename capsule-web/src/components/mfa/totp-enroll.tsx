/**
 * TOTP enrollment flow:
 * 1. Call POST /auth/totp/enroll to get provisioning_uri
 * 2. Show QR code and provisioning URI
 * 3. User scans with their authenticator app
 * 4. User enters a code to confirm enrollment
 * 5. Call POST /auth/totp/verify-enrollment with the code
 */

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ApiError, totpEnroll, totpVerifyEnrollment } from '@/lib/api';
import { useState } from 'react';
import QRCode from 'react-qr-code';

interface TotpEnrollProps {
    onSuccess: () => void;
    onCancel: () => void;
}

type EnrollStep = 'start' | 'scan' | 'verify';

export function TotpEnroll({ onSuccess, onCancel }: TotpEnrollProps) {
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
                    : 'Failed to start enrollment.',
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
                    : 'Invalid code. Please try again.',
            );
        } finally {
            setLoading(false);
        }
    }

    if (step === 'start') {
        return (
            <div className="space-y-4">
                <p className="text-sm text-muted-foreground">
                    Use an authenticator app (e.g. Google Authenticator, Authy)
                    to scan a QR code and generate one-time codes.
                </p>
                <div className="flex gap-2">
                    <Button onClick={handleStart} disabled={loading}>
                        {loading ? 'Starting…' : 'Set up authenticator'}
                    </Button>
                    <Button variant="ghost" onClick={onCancel}>
                        Cancel
                    </Button>
                </div>
            </div>
        );
    }

    if (step === 'scan') {
        return (
            <div className="space-y-4">
                <p className="text-sm text-muted-foreground">
                    Scan this QR code with your authenticator app, then enter
                    the 6-digit code to confirm.
                </p>
                <div className="flex justify-center p-4 bg-white rounded-md">
                    <QRCode value={provisioningUri} size={180} />
                </div>
                <details className="text-xs text-muted-foreground">
                    <summary className="cursor-pointer select-none">
                        Can't scan? Show setup key
                    </summary>
                    <p className="mt-1 break-all font-mono">
                        {provisioningUri}
                    </p>
                </details>
                <Button onClick={() => setStep('verify')} className="w-full">
                    I've scanned the code
                </Button>
            </div>
        );
    }

    return (
        <form onSubmit={handleVerify} className="space-y-4">
            <p className="text-sm text-muted-foreground">
                Enter the 6-digit code from your authenticator app to complete
                setup.
            </p>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <div className="grid gap-2">
                <Label htmlFor="totp-verify">Verification Code</Label>
                <Input
                    id="totp-verify"
                    type="text"
                    inputMode="numeric"
                    placeholder="123456"
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
                    {loading ? 'Verifying…' : 'Confirm'}
                </Button>
                <Button
                    variant="ghost"
                    type="button"
                    onClick={() => setStep('scan')}
                >
                    Back
                </Button>
            </div>
        </form>
    );
}
