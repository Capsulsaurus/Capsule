/**
 * Single source of truth for terms that must never be auto-translated.
 *
 * Browser/machine translators (Chrome/Google Translate, Edge, Safari, Firefox)
 * mangle technical names unless the surrounding element opts out via the
 * `translate="no"` attribute. The {@link file://./rehype-notranslate.mjs} plugin
 * consumes this list to wrap bare-prose occurrences of these terms; all
 * backticked code (`<code>`/`<pre>`) is protected automatically and needs no
 * entry here.
 *
 * Keep entries as they appear verbatim in the docs (matching is case-sensitive).
 * Order within a category does not matter — the plugin sorts longest-first so a
 * specific term (e.g. `ML-KEM-768`) always wins over its prefix (`ML-KEM`).
 *
 * Sourced from design/cryptography/primitives.md (the primitives SSoT) plus the
 * protocol/infra inventory across the docs. When a new primitive or product name
 * enters the docs, add it here — this is the only place to edit.
 */

/** Cryptographic primitives & algorithms. */
const CRYPTO = [
    'SHA-256',
    'SHA-512',
    'SHA-2',
    'SHA-3',
    'HKDF-SHA512',
    'HKDF',
    'Argon2id',
    'AES-256-GCM',
    'AES-GCM',
    'AES-NI',
    'ChaCha20-Poly1305',
    'Ed25519',
    'EdDSA',
    'ML-DSA-65',
    'ML-DSA',
    'X-Wing',
    'X25519',
    'ML-KEM-768',
    'ML-KEM',
    'MLS',
    'OpenMLS',
    'HPKE',
    'STREAM',
    'CSPRNG',
    'BLAKE3',
    'getrandom',
    'TLS 1.3',
    'ECDHE',
];

/** Brand & product names. */
const BRAND = ['Capsule'];

/** Protocol, format & infrastructure names. */
const INFRA = [
    'GraphQL',
    'gRPC',
    'WebSocket',
    'ProtoBuf',
    'Protobuf',
    'OIDC',
    'OpenID Connect',
    'JWT',
    'OAuth',
    'CBOR',
    'TUS',
    'PostgreSQL',
    'Postgres',
    'Valkey',
    'MinIO',
    'Kubernetes',
    'RKE2',
    'Cilium',
    'Calico',
    'rustls',
    'glibc',
];

/** Flat, de-duplicated list of every protected term. */
export const NO_TRANSLATE_TERMS = [...new Set([...CRYPTO, ...BRAND, ...INFRA])];
