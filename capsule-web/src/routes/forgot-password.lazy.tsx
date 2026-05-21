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
import { ApiError, requestPasswordReset } from '@/lib/api';
import { Link, createLazyFileRoute } from '@tanstack/react-router';
import { MountainIcon } from 'lucide-react';
import type React from 'react';
import { useState } from 'react';

export const Route = createLazyFileRoute('/forgot-password')({
    component: ForgotPassword,
});

function ForgotPassword() {
    const [email, setEmail] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);
    const [submitted, setSubmitted] = useState(false);

    async function handleSubmit(e: React.FormEvent) {
        e.preventDefault();
        setError(null);
        setLoading(true);
        try {
            await requestPasswordReset(email);
            setSubmitted(true);
        } catch (err) {
            // Always show success to prevent email enumeration
            if (err instanceof ApiError && err.status !== 200) {
                setSubmitted(true);
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
                    <CardTitle className="text-2xl">Forgot Password</CardTitle>
                    <CardDescription>
                        {submitted
                            ? 'Check your email for instructions.'
                            : "Enter your email and we'll send you a reset link."}
                    </CardDescription>
                </CardHeader>
                {!submitted ? (
                    <form onSubmit={handleSubmit}>
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
                        </CardContent>
                        <CardFooter className="flex flex-col gap-3">
                            <Button
                                className="w-full"
                                type="submit"
                                disabled={loading}
                            >
                                {loading ? 'Sending…' : 'Send reset link'}
                            </Button>
                            <Link
                                to="/login"
                                className="text-xs text-muted-foreground underline"
                            >
                                Back to login
                            </Link>
                        </CardFooter>
                    </form>
                ) : (
                    <CardFooter className="flex flex-col gap-3 pt-4">
                        <p className="text-sm text-muted-foreground text-center">
                            If an account with that email exists, you'll receive
                            a reset link shortly.
                        </p>
                        <Link
                            to="/login"
                            className="text-xs text-muted-foreground underline"
                        >
                            Back to login
                        </Link>
                    </CardFooter>
                )}
            </Card>
        </div>
    );
}
