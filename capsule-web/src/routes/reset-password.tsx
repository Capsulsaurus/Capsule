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
import { ApiError, resetPassword } from '@/lib/api';
import { Link, createFileRoute } from '@tanstack/react-router';
import { MountainIcon } from 'lucide-react';
import type React from 'react';
import { useState } from 'react';
import { z } from 'zod';

const resetPasswordSearchSchema = z.object({
    token: z.string().optional(),
});

export const Route = createFileRoute('/reset-password')({
    validateSearch: resetPasswordSearchSchema,
    component: ResetPassword,
});

function ResetPassword() {
    const { token } = Route.useSearch();
    const [newPassword, setNewPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);
    const [success, setSuccess] = useState(false);

    if (!token) {
        return (
            <div className="flex flex-col items-center justify-center min-h-screen bg-muted/40 p-4">
                <Card className="w-full max-w-sm">
                    <CardHeader>
                        <CardTitle className="text-2xl">Invalid Link</CardTitle>
                        <CardDescription>
                            This password reset link is invalid or has expired.
                        </CardDescription>
                    </CardHeader>
                    <CardFooter>
                        <Link
                            to="/forgot-password"
                            className="text-sm underline"
                        >
                            Request a new reset link
                        </Link>
                    </CardFooter>
                </Card>
            </div>
        );
    }

    async function handleSubmit(e: React.FormEvent) {
        e.preventDefault();
        if (!token) return;
        if (newPassword !== confirmPassword) {
            setError('Passwords do not match.');
            return;
        }
        if (newPassword.length < 8) {
            setError('Password must be at least 8 characters.');
            return;
        }
        setError(null);
        setLoading(true);
        try {
            await resetPassword(token, newPassword);
            setSuccess(true);
        } catch (err) {
            if (err instanceof ApiError) {
                setError(err.message);
            } else {
                setError('Something went wrong. Please try again.');
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
            <Card className="w-full max-w-sm">
                <CardHeader>
                    <CardTitle className="text-2xl">Reset Password</CardTitle>
                    <CardDescription>
                        {success
                            ? 'Your password has been reset.'
                            : 'Enter your new password.'}
                    </CardDescription>
                </CardHeader>
                {!success ? (
                    <form onSubmit={handleSubmit}>
                        <CardContent className="grid gap-4">
                            {error && (
                                <p className="text-sm text-destructive">
                                    {error}
                                </p>
                            )}
                            <div className="grid gap-2">
                                <Label htmlFor="new-password">
                                    New Password
                                </Label>
                                <Input
                                    id="new-password"
                                    type="password"
                                    required
                                    minLength={8}
                                    value={newPassword}
                                    onChange={(e) =>
                                        setNewPassword(e.target.value)
                                    }
                                    disabled={loading}
                                />
                            </div>
                            <div className="grid gap-2">
                                <Label htmlFor="confirm-password">
                                    Confirm Password
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
                        </CardContent>
                        <CardFooter className="flex flex-col gap-3">
                            <Button
                                className="w-full"
                                type="submit"
                                disabled={loading}
                            >
                                {loading ? 'Resetting…' : 'Reset password'}
                            </Button>
                        </CardFooter>
                    </form>
                ) : (
                    <CardFooter className="flex flex-col gap-3 pt-4">
                        <Link to="/login">
                            <Button className="w-full">Sign in</Button>
                        </Link>
                    </CardFooter>
                )}
            </Card>
        </div>
    );
}
