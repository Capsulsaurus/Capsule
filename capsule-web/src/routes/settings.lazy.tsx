import { useQueryClient } from '@tanstack/react-query';
import { createLazyFileRoute, Link } from '@tanstack/react-router';
import type React from 'react';
import { useEffect, useState } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
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
import { ApiError, changePassword, updateProfile } from '@/lib/api';
import { useAuth } from '@/lib/auth-context';

export const Route = createLazyFileRoute('/settings')({
    component: Settings,
});

function Settings() {
    const intl = useIntl();
    const { user } = useAuth();
    const queryClient = useQueryClient();

    const [displayName, setDisplayName] = useState('');
    const [currentPassword, setCurrentPassword] = useState('');
    const [newPassword, setNewPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);

    useEffect(() => {
        if (user) {
            setDisplayName(user.display_name ?? '');
        }
    }, [user]);

    async function handleProfileSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setSuccess(null);
        setLoading(true);
        try {
            // A blank field is a request to clear the name, which is an explicit `null` on the
            // wire — an absent key would mean "leave it alone" and the form would be unable to
            // remove a name it just showed.
            const updated = await updateProfile({
                display_name: displayName.trim() === '' ? null : displayName,
            });
            queryClient.setQueryData(['auth', 'profile'], updated);
            setSuccess(intl.formatMessage({ id: 'settings.profile_updated' }));
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({
                          id: 'settings.profile_update_failed',
                      }),
            );
        } finally {
            setLoading(false);
        }
    }

    async function handlePasswordSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setSuccess(null);
        if (newPassword !== confirmPassword) {
            setError(intl.formatMessage({ id: 'settings.password_mismatch' }));
            return;
        }
        if (newPassword.length < 12) {
            setError(intl.formatMessage({ id: 'common.password_min' }));
            return;
        }
        setLoading(true);
        try {
            // Its own operation, not a profile edit: it ends every *other* session of the
            // account and re-opens this one, so it is a credential rotation rather than a field
            // change (`S-C54`).
            await changePassword(currentPassword, newPassword);
            setSuccess(intl.formatMessage({ id: 'settings.password_updated' }));
            setCurrentPassword('');
            setNewPassword('');
            setConfirmPassword('');
        } catch (err) {
            setError(
                err instanceof ApiError
                    ? err.message
                    : intl.formatMessage({
                          id: 'settings.password_update_failed',
                      }),
            );
        } finally {
            setLoading(false);
        }
    }

    return (
        <div className="max-w-2xl mx-auto p-6 space-y-8">
            <div className="flex items-center justify-between">
                <h1 className="text-2xl font-bold">
                    <FormattedMessage id="settings.title" />
                </h1>
                <Link
                    to="/settings/security"
                    className="text-sm underline text-muted-foreground"
                >
                    <FormattedMessage id="settings.security_link" />
                </Link>
            </div>

            <Card>
                <CardHeader>
                    <CardTitle>
                        <FormattedMessage id="settings.profile.title" />
                    </CardTitle>
                    <CardDescription>
                        <FormattedMessage id="settings.profile.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent>
                    <form onSubmit={handleProfileSubmit} className="space-y-4">
                        {error && (
                            <p className="text-sm text-destructive">{error}</p>
                        )}
                        {success && (
                            <p className="text-sm text-green-600">{success}</p>
                        )}
                        <div className="grid gap-2">
                            <Label htmlFor="display_name">
                                <FormattedMessage id="common.display_name" />
                            </Label>
                            <Input
                                id="display_name"
                                value={displayName}
                                onChange={(e) => setDisplayName(e.target.value)}
                                disabled={loading}
                            />
                        </div>
                        <div className="grid gap-2">
                            <Label htmlFor="email">
                                <FormattedMessage id="common.email" />
                            </Label>
                            {/* Read-only. Changing a login address needs proof the caller
                                controls the new one, and this deployment has no way to obtain
                                it — so the server has no field for it rather than a field it
                                refuses (`S-C54`). */}
                            <Input
                                id="email"
                                type="email"
                                value={user?.email ?? ''}
                                readOnly
                                disabled
                            />
                        </div>
                        <Button type="submit" disabled={loading}>
                            {loading ? (
                                <FormattedMessage id="common.saving" />
                            ) : (
                                <FormattedMessage id="common.save_changes" />
                            )}
                        </Button>
                    </form>
                </CardContent>
            </Card>

            <Card>
                <CardHeader>
                    <CardTitle>
                        <FormattedMessage id="settings.password.title" />
                    </CardTitle>
                    <CardDescription>
                        <FormattedMessage id="settings.password.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent>
                    <form onSubmit={handlePasswordSubmit} className="space-y-4">
                        <div className="grid gap-2">
                            <Label htmlFor="current-password">
                                <FormattedMessage id="settings.current_password" />
                            </Label>
                            <Input
                                id="current-password"
                                type="password"
                                required
                                value={currentPassword}
                                onChange={(e) =>
                                    setCurrentPassword(e.target.value)
                                }
                                disabled={loading}
                            />
                        </div>
                        <div className="grid gap-2">
                            <Label htmlFor="new-password">
                                <FormattedMessage id="auth.new_password" />
                            </Label>
                            <Input
                                id="new-password"
                                type="password"
                                required
                                minLength={8}
                                value={newPassword}
                                onChange={(e) => setNewPassword(e.target.value)}
                                disabled={loading}
                            />
                        </div>
                        <div className="grid gap-2">
                            <Label htmlFor="confirm-password">
                                <FormattedMessage id="settings.confirm_new_password" />
                            </Label>
                            <Input
                                id="confirm-password"
                                type="password"
                                required
                                value={confirmPassword}
                                onChange={(e) =>
                                    setConfirmPassword(e.target.value)
                                }
                                disabled={loading}
                            />
                        </div>
                        <Button type="submit" disabled={loading}>
                            {loading ? (
                                <FormattedMessage id="settings.updating" />
                            ) : (
                                <FormattedMessage id="settings.update_password" />
                            )}
                        </Button>
                    </form>
                </CardContent>
            </Card>
        </div>
    );
}
