/**
 * Passkey registration flow:
 * 1. Call POST /auth/passkey/register/start to get creation options
 * 2. Invoke browser navigator.credentials.create() with those options
 * 3. Call POST /auth/passkey/register/finish with the credential + optional name
 */

import { KeyRoundIcon } from 'lucide-react';
import { useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import {
    ApiError,
    passkeyRegisterFinish,
    passkeyRegisterStart,
} from '@/lib/api';
import { registerPasskey } from '@/lib/webauthn';

interface PasskeyRegisterProps {
    onSuccess: () => void;
    onCancel: () => void;
}

export function PasskeyRegister({ onSuccess, onCancel }: PasskeyRegisterProps) {
    const intl = useIntl();
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
                setError(
                    intl.formatMessage({ id: 'mfa.passkey.reg_cancelled' }),
                );
            } else if (
                err instanceof Error &&
                err.name === 'InvalidStateError'
            ) {
                setError(
                    intl.formatMessage({
                        id: 'mfa.passkey.already_registered',
                    }),
                );
            } else {
                setError(intl.formatMessage({ id: 'mfa.passkey.reg_failed' }));
            }
        } finally {
            setLoading(false);
        }
    }

    return (
        <form onSubmit={handleRegister} className="space-y-4">
            <p className="text-sm text-muted-foreground">
                <FormattedMessage id="mfa.passkey.description" />
            </p>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <div className="grid gap-2">
                <Label htmlFor="passkey-name">
                    <FormattedMessage id="mfa.passkey.name_label" />
                </Label>
                <Input
                    id="passkey-name"
                    type="text"
                    placeholder={intl.formatMessage({
                        id: 'mfa.passkey.name_placeholder',
                    })}
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    disabled={loading}
                />
            </div>
            <div className="flex gap-2">
                <Button type="submit" disabled={loading}>
                    <KeyRoundIcon className="mr-2 h-4 w-4" />
                    {loading ? (
                        <FormattedMessage id="mfa.passkey.registering" />
                    ) : (
                        <FormattedMessage id="mfa.passkey.create" />
                    )}
                </Button>
                <Button variant="ghost" type="button" onClick={onCancel}>
                    <FormattedMessage id="common.cancel" />
                </Button>
            </div>
        </form>
    );
}
