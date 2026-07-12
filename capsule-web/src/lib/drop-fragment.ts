// Upload-link URL fragment parsing (slice S-D3).
//
// The upload link is `https://server.tld/u/{opaque-id}#{fragment}`. Browsers never transmit the
// fragment, so it carries every secret the guest needs and the server never sees (SSoT: the Web
// Upload design doc — Server-blind). The fragment holds the Drop Key public half and, for a
// passphrase-gated link, the Argon2id salt + parameters the guest needs to compute its possession
// proof entirely client-side (the passphrase itself is never encoded here — only the guest types
// it).
//
// Fragment grammar (`~` is an RFC 3986 unreserved char, absent from the base64url alphabet, so it
// is an unambiguous field separator):
//
//   plain:            <dropPubkeyB64url>
//   passphrase-gated: <dropPubkeyB64url>~<memKib>~<tCost>~<pCost>~<saltHex>

/** X-Wing Drop Key public-half length (`pk_M` 1184 ‖ `pk_X` 32) — SSoT: `DEK_PUBLIC_LEN`. */
const DROP_PUBKEY_LEN = 1184 + 32;

/** The Argon2id parameters + salt a passphrase-gated link delivers in its fragment. */
export interface DropPassphraseParams {
    saltHex: string;
    memKib: number;
    tCost: number;
    pCost: number;
}

/** A parsed upload link: the Drop Key bytes and, if the link is passphrase-gated, its params. */
export interface DropLink {
    dropPubkey: Uint8Array;
    passphrase: DropPassphraseParams | null;
}

/** Decode base64url (no padding required) to bytes, or null on a malformed input. */
function b64urlToBytes(value: string): Uint8Array | null {
    const normalized = value.replace(/-/g, '+').replace(/_/g, '/');
    const padded = normalized.padEnd(
        normalized.length + ((4 - (normalized.length % 4)) % 4),
        '=',
    );
    try {
        const binary = atob(padded);
        const out = new Uint8Array(binary.length);
        for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
        return out;
    } catch {
        return null;
    }
}

/** True if `s` is a non-empty lowercase-hex string of even length. */
function isLowerHex(s: string): boolean {
    return s.length > 0 && s.length % 2 === 0 && /^[0-9a-f]+$/.test(s);
}

/**
 * Parse a raw URL fragment (with any leading `#` already stripped) into a {@link DropLink}, or
 * return null if it is missing, malformed, or does not carry a well-formed Drop Key — the route
 * renders that as a single generic "incomplete link" state.
 */
export function parseDropFragment(fragment: string): DropLink | null {
    const trimmed = fragment.trim();
    if (!trimmed) return null;

    const parts = trimmed.split('~');
    const dropPubkey = b64urlToBytes(parts[0]);
    if (!dropPubkey || dropPubkey.length !== DROP_PUBKEY_LEN) return null;

    if (parts.length === 1) {
        return { dropPubkey, passphrase: null };
    }
    // A passphrase-gated link carries exactly four extra fields: mem, t, p, salt.
    if (parts.length !== 5) return null;
    const memKib = Number(parts[1]);
    const tCost = Number(parts[2]);
    const pCost = Number(parts[3]);
    const saltHex = parts[4];
    const posInt = (n: number) => Number.isInteger(n) && n > 0;
    if (
        !posInt(memKib) ||
        !posInt(tCost) ||
        !posInt(pCost) ||
        !isLowerHex(saltHex)
    ) {
        return null;
    }
    return { dropPubkey, passphrase: { saltHex, memKib, tCost, pCost } };
}
