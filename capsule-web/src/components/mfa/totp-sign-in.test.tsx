// What a failed sign-in shows, for both the password login and the identity-provider callback:
// the catalog message for a stable server `error.*` code, else the server's English detail,
// else the generic fallback.

import { describe, expect, test } from 'bun:test';
import { createIntl } from 'react-intl';

import { ApiError } from '@/lib/api';
import { signInErrorText } from './totp-sign-in';

const intl = createIntl({
    locale: 'en',
    messages: {
        'error.auth.oidc_state_invalid':
            'That sign-in has expired. Start again.',
        'auth.error.unexpected': 'An unexpected error occurred.',
    },
});

describe('signInErrorText', () => {
    test('renders a known error code from the catalog, not the detail', () => {
        const err = new ApiError(
            400,
            'state is unknown or expired',
            'error.auth.oidc_state_invalid',
        );
        expect(signInErrorText(intl, err)).toBe(
            'That sign-in has expired. Start again.',
        );
    });

    test('falls back to the English detail for a code the catalog lacks', () => {
        const err = new ApiError(
            400,
            'something new',
            'error.auth.not_shipped',
        );
        expect(signInErrorText(intl, err)).toBe('something new');
    });

    test('falls back to the English detail when the server sent no code', () => {
        expect(signInErrorText(intl, new ApiError(500, 'plain detail'))).toBe(
            'plain detail',
        );
    });

    test('shows the generic message for anything that is not a server answer', () => {
        expect(signInErrorText(intl, new TypeError('Failed to fetch'))).toBe(
            'An unexpected error occurred.',
        );
    });
});
