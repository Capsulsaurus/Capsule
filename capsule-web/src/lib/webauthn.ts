/**
 * WebAuthn browser API helpers for passkey login and registration.
 * Handles base64url <-> ArrayBuffer conversion needed by the browser API.
 */

function base64urlToBuffer(base64url: string): ArrayBuffer {
    const base64 = base64url.replace(/-/g, '+').replace(/_/g, '/');
    const padded = base64.padEnd(
        base64.length + ((4 - (base64.length % 4)) % 4),
        '=',
    );
    const binary = atob(padded);
    const buffer = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
        buffer[i] = binary.charCodeAt(i);
    }
    return buffer.buffer;
}

function bufferToBase64url(buffer: ArrayBuffer): string {
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (const byte of bytes) {
        binary += String.fromCharCode(byte);
    }
    return btoa(binary)
        .replace(/\+/g, '-')
        .replace(/\//g, '_')
        .replace(/=/g, '');
}

/** Transform server-side creation options (base64url strings) to browser-expected formats */
function prepareCreationOptions(
    options: Record<string, unknown>,
): PublicKeyCredentialCreationOptions {
    const pk = options.publicKey as Record<string, unknown>;
    return {
        ...pk,
        challenge: base64urlToBuffer(pk.challenge as string),
        user: {
            ...(pk.user as Record<string, unknown>),
            id: base64urlToBuffer(
                (pk.user as Record<string, unknown>).id as string,
            ),
        },
        excludeCredentials: (
            (pk.excludeCredentials as Array<Record<string, unknown>>) ?? []
        ).map((cred) => ({
            ...cred,
            id: base64urlToBuffer(cred.id as string),
        })),
    } as unknown as PublicKeyCredentialCreationOptions;
}

/** Transform server-side request options (base64url strings) to browser-expected formats */
function prepareRequestOptions(
    options: Record<string, unknown>,
): PublicKeyCredentialRequestOptions {
    const pk = options.publicKey as Record<string, unknown>;
    return {
        ...pk,
        challenge: base64urlToBuffer(pk.challenge as string),
        allowCredentials: (
            (pk.allowCredentials as Array<Record<string, unknown>>) ?? []
        ).map((cred) => ({
            ...cred,
            id: base64urlToBuffer(cred.id as string),
        })),
    } as unknown as PublicKeyCredentialRequestOptions;
}

/** Serialize a PublicKeyCredential to a plain JSON-serializable object */
function serializeCredential(cred: PublicKeyCredential): unknown {
    const response = cred.response;
    if (response instanceof AuthenticatorAttestationResponse) {
        return {
            id: cred.id,
            rawId: bufferToBase64url(cred.rawId),
            type: cred.type,
            response: {
                attestationObject: bufferToBase64url(
                    response.attestationObject,
                ),
                clientDataJSON: bufferToBase64url(response.clientDataJSON),
            },
        };
    }
    if (response instanceof AuthenticatorAssertionResponse) {
        return {
            id: cred.id,
            rawId: bufferToBase64url(cred.rawId),
            type: cred.type,
            response: {
                authenticatorData: bufferToBase64url(
                    response.authenticatorData,
                ),
                clientDataJSON: bufferToBase64url(response.clientDataJSON),
                signature: bufferToBase64url(response.signature),
                userHandle: response.userHandle
                    ? bufferToBase64url(response.userHandle)
                    : null,
            },
        };
    }
    throw new Error('Unknown credential response type');
}

/** Trigger passkey authentication and return serialized credential */
export async function authenticateWithPasskey(
    requestOptions: unknown,
): Promise<unknown> {
    const options = prepareRequestOptions(
        requestOptions as Record<string, unknown>,
    );
    const credential = await navigator.credentials.get({ publicKey: options });
    if (!credential) throw new Error('No credential returned');
    return serializeCredential(credential as PublicKeyCredential);
}

/** Trigger passkey registration and return serialized credential */
export async function registerPasskey(
    creationOptions: unknown,
): Promise<unknown> {
    const options = prepareCreationOptions(
        creationOptions as Record<string, unknown>,
    );
    const credential = await navigator.credentials.create({
        publicKey: options,
    });
    if (!credential) throw new Error('No credential returned');
    return serializeCredential(credential as PublicKeyCredential);
}
