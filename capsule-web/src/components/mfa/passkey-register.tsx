/**
 * Passkey registration flow:
 * 1. Call POST /auth/passkey/register/start to get creation options
 * 2. Invoke browser navigator.credentials.create() with those options
 * 3. Call POST /auth/passkey/register/finish with the credential + optional name
 */

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import {
    ApiError,
    passkeyRegisterFinish,
    passkeyRegisterStart,
} from '@/lib/api';
import { registerPasskey } from '@/lib/webauthn';
import { KeyRoundIcon } from 'lucide-react';
import { useState } from 'react';

interface PasskeyRegisterProps {
    onSuccess: () => void;
    onCancel: () => void;
}

export function PasskeyRegister({ onSuccess, onCancel }: PasskeyRegisterProps) {
    const [name, setName] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    async function handleRegister(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            const options = await passkeyRegisterStart();
            const credential = await registerPasskey(options);
            await passkeyRegisterFinish(credential, name || undefined);
            onSuccess();
        } catch (err) {
            if (err instanceof ApiError) {
                setError(err.message);
            } else if (err instanceof Error && err.name === 'NotAllowedError') {
                setError('Passkey registration was cancelled.');
            } else if (
                err instanceof Error &&
                err.name === 'InvalidStateError'
            ) {
                setError('A passkey for this device is already registered.');
            } else {
                setError('Passkey registration failed.');
            }
        } finally {
            setLoading(false);
        }
    }

    return (
        <form onSubmit={handleRegister} className="space-y-4">
            <p className="text-sm text-muted-foreground">
                Passkeys use your device's biometrics or PIN to sign in securely
                without a password.
            </p>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <div className="grid gap-2">
                <Label htmlFor="passkey-name">Passkey Name (optional)</Label>
                <Input
                    id="passkey-name"
                    type="text"
                    placeholder="My MacBook"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    disabled={loading}
                />
            </div>
            <div className="flex gap-2">
                <Button type="submit" disabled={loading}>
                    <KeyRoundIcon className="mr-2 h-4 w-4" />
                    {loading ? 'Registering…' : 'Create Passkey'}
                </Button>
                <Button variant="ghost" type="button" onClick={onCancel}>
                    Cancel
                </Button>
            </div>
        </form>
    );
}
