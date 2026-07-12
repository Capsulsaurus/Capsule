import { useQuery, useQueryClient } from '@tanstack/react-query';
import { createLazyFileRoute, Link } from '@tanstack/react-router';
import type React from 'react';
import { useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import { PasskeyRegister } from '@/components/mfa/passkey-register';
import { TotpEnroll } from '@/components/mfa/totp-enroll';
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
import {
    ApiError,
    type Device,
    deletePasskey,
    getDevices,
    listPasskeys,
    type PasskeyCredential,
    totpDisable,
} from '@/lib/api';

export const Route = createLazyFileRoute('/settings/security')({
    component: SecuritySettings,
});

function formatDate(unixSecs: number) {
    return new Date(unixSecs * 1000).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
    });
}

function DeviceCard({ device }: { device: Device }) {
    const intl = useIntl();
    return (
        <div className="flex items-start justify-between p-3 rounded-md border">
            <div className="space-y-1 text-sm">
                <div className="font-medium">
                    {device.user_agent ??
                        intl.formatMessage({ id: 'security.unknown_device' })}
                    {device.is_current && (
                        <span className="ml-2 text-xs text-green-600 font-normal">
                            <FormattedMessage id="security.this_device" />
                        </span>
                    )}
                </div>
                {device.ip_address && (
                    <div className="text-muted-foreground text-xs">
                        {device.ip_address}
                    </div>
                )}
                <div className="text-muted-foreground text-xs">
                    <FormattedMessage
                        id="security.last_active"
                        values={{ date: formatDate(device.last_active_at) }}
                    />
                </div>
            </div>
        </div>
    );
}

function PasskeyRow({
    passkey,
    onDeleted,
}: {
    passkey: PasskeyCredential;
    onDeleted: () => void;
}) {
    const intl = useIntl();
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);

    async function handleDelete() {
        if (
            !confirm(
                intl.formatMessage(
                    { id: 'security.delete_passkey_confirm' },
                    { name: passkey.name },
                ),
            )
        )
            return;
        setLoading(true);
        setError(null);
        try {
            await deletePasskey(passkey.id);
            onDeleted();
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({
                          id: 'security.delete_passkey_failed',
                      }),
            );
        } finally {
            setLoading(false);
        }
    }

    return (
        <div className="flex items-center justify-between p-3 rounded-md border">
            <div className="space-y-1 text-sm">
                <div className="font-medium">{passkey.name}</div>
                <div className="text-muted-foreground text-xs">
                    <FormattedMessage
                        id="security.added"
                        values={{ date: formatDate(passkey.created_at) }}
                    />
                </div>
                {error && (
                    <div className="text-xs text-destructive">{error}</div>
                )}
            </div>
            <Button
                variant="destructive"
                size="sm"
                onClick={handleDelete}
                disabled={loading}
            >
                {loading ? (
                    <FormattedMessage id="security.deleting" />
                ) : (
                    <FormattedMessage id="common.remove" />
                )}
            </Button>
        </div>
    );
}

function SecuritySettings() {
    const intl = useIntl();
    const queryClient = useQueryClient();

    const { data: devices, isLoading: devicesLoading } = useQuery({
        queryKey: ['auth', 'devices'],
        queryFn: getDevices,
    });

    const { data: passkeys, isLoading: passkeysLoading } = useQuery({
        queryKey: ['auth', 'passkeys'],
        queryFn: listPasskeys,
    });

    const [showTotpEnroll, setShowTotpEnroll] = useState(false);
    const [showPasskeyRegister, setShowPasskeyRegister] = useState(false);
    const [totpDisableCode, setTotpDisableCode] = useState('');
    const [totpDisableError, setTotpDisableError] = useState<string | null>(
        null,
    );
    const [totpDisableLoading, setTotpDisableLoading] = useState(false);
    const [totpSuccess, setTotpSuccess] = useState<string | null>(null);

    async function handleTotpDisable(e: React.FormEvent) {
        e.preventDefault();
        setTotpDisableError(null);
        setTotpDisableLoading(true);
        try {
            await totpDisable(totpDisableCode);
            setTotpSuccess(
                intl.formatMessage({ id: 'security.totp_disabled' }),
            );
            setTotpDisableCode('');
            setShowTotpEnroll(false);
        } catch (err) {
            setTotpDisableError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({
                          id: 'security.totp_disable_failed',
                      }),
            );
        } finally {
            setTotpDisableLoading(false);
        }
    }

    return (
        <div className="max-w-2xl mx-auto p-6 space-y-8">
            <div className="flex items-center justify-between">
                <h1 className="text-2xl font-bold">
                    <FormattedMessage id="security.title" />
                </h1>
                <Link
                    to="/settings"
                    className="text-sm underline text-muted-foreground"
                >
                    <FormattedMessage id="security.profile_link" />
                </Link>
            </div>

            {/* Active Sessions */}
            <Card>
                <CardHeader>
                    <CardTitle>
                        <FormattedMessage id="security.sessions.title" />
                    </CardTitle>
                    <CardDescription>
                        <FormattedMessage id="security.sessions.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-2">
                    {devicesLoading && (
                        <p className="text-sm text-muted-foreground">
                            <FormattedMessage id="security.sessions.loading" />
                        </p>
                    )}
                    {devices?.map((device) => (
                        <DeviceCard key={device.id} device={device} />
                    ))}
                    {!devicesLoading && (!devices || devices.length === 0) && (
                        <p className="text-sm text-muted-foreground">
                            <FormattedMessage id="security.sessions.empty" />
                        </p>
                    )}
                </CardContent>
            </Card>

            {/* TOTP */}
            <Card>
                <CardHeader>
                    <CardTitle>
                        <FormattedMessage id="security.totp.title" />
                    </CardTitle>
                    <CardDescription>
                        <FormattedMessage id="security.totp.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                    {totpSuccess && (
                        <p className="text-sm text-green-600">{totpSuccess}</p>
                    )}
                    {showTotpEnroll ? (
                        <TotpEnroll
                            onSuccess={() => {
                                setShowTotpEnroll(false);
                                setTotpSuccess(
                                    intl.formatMessage({
                                        id: 'security.totp.enabled',
                                    }),
                                );
                            }}
                            onCancel={() => setShowTotpEnroll(false)}
                        />
                    ) : (
                        <div className="space-y-4">
                            <Button
                                onClick={() => {
                                    setTotpSuccess(null);
                                    setShowTotpEnroll(true);
                                }}
                            >
                                <FormattedMessage id="security.totp.setup_button" />
                            </Button>
                            <div className="border-t pt-4">
                                <p className="text-sm text-muted-foreground mb-2">
                                    <FormattedMessage id="security.totp.disable_prompt" />
                                </p>
                                <form
                                    onSubmit={handleTotpDisable}
                                    className="flex gap-2"
                                >
                                    <div className="grid gap-1 flex-1">
                                        <Label
                                            htmlFor="totp-disable-code"
                                            className="sr-only"
                                        >
                                            <FormattedMessage id="security.totp.code_label" />
                                        </Label>
                                        <Input
                                            id="totp-disable-code"
                                            type="text"
                                            inputMode="numeric"
                                            placeholder={intl.formatMessage({
                                                id: 'security.totp.code_placeholder',
                                            })}
                                            maxLength={6}
                                            value={totpDisableCode}
                                            onChange={(e) =>
                                                setTotpDisableCode(
                                                    e.target.value,
                                                )
                                            }
                                            disabled={totpDisableLoading}
                                        />
                                    </div>
                                    <Button
                                        type="submit"
                                        variant="destructive"
                                        disabled={
                                            totpDisableLoading ||
                                            !totpDisableCode
                                        }
                                    >
                                        {totpDisableLoading ? (
                                            <FormattedMessage id="security.totp.disabling" />
                                        ) : (
                                            <FormattedMessage id="security.totp.disable_button" />
                                        )}
                                    </Button>
                                </form>
                                {totpDisableError && (
                                    <p className="text-sm text-destructive mt-1">
                                        {totpDisableError}
                                    </p>
                                )}
                            </div>
                        </div>
                    )}
                </CardContent>
            </Card>

            {/* Passkeys */}
            <Card>
                <CardHeader>
                    <CardTitle>
                        <FormattedMessage id="security.passkeys.title" />
                    </CardTitle>
                    <CardDescription>
                        <FormattedMessage id="security.passkeys.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-3">
                    {passkeysLoading && (
                        <p className="text-sm text-muted-foreground">
                            <FormattedMessage id="security.passkeys.loading" />
                        </p>
                    )}
                    {passkeys?.map((passkey) => (
                        <PasskeyRow
                            key={passkey.id}
                            passkey={passkey}
                            onDeleted={() =>
                                queryClient.invalidateQueries({
                                    queryKey: ['auth', 'passkeys'],
                                })
                            }
                        />
                    ))}
                    {!passkeysLoading &&
                        (!passkeys || passkeys.length === 0) && (
                            <p className="text-sm text-muted-foreground">
                                <FormattedMessage id="security.passkeys.empty" />
                            </p>
                        )}
                    {showPasskeyRegister ? (
                        <PasskeyRegister
                            onSuccess={() => {
                                setShowPasskeyRegister(false);
                                queryClient.invalidateQueries({
                                    queryKey: ['auth', 'passkeys'],
                                });
                            }}
                            onCancel={() => setShowPasskeyRegister(false)}
                        />
                    ) : (
                        <Button
                            variant="outline"
                            onClick={() => setShowPasskeyRegister(true)}
                        >
                            <FormattedMessage id="security.passkeys.add_button" />
                        </Button>
                    )}
                </CardContent>
            </Card>
        </div>
    );
}
