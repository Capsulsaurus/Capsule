// The libraries and tools the docs name, each with its source repository.
// src/lib/remark-library-links.mjs links the first mention of each on a page,
// so an author writes the name and the link follows. Pure data.
//
// `names` are matched in prose as whole words, case-sensitively; `code` are
// exact inline-code spellings (a crate or package name) that count as a
// mention too. A name that is also an ordinary word, or that names two
// different things, is never matched: it is listed in EXPLICIT_ONLY with the
// reason, and an author who means the library links it by hand.

/**
 * @typedef {object} Library
 * @property {string} url Source repository.
 * @property {string[]} names Prose spellings.
 * @property {string[]} [code] Inline-code spellings.
 */

/** @type {Library[]} */
export const LIBRARIES = [
    // Sibling projects
    {
        url: 'https://github.com/getkono/kynos',
        names: ['Kynos'],
        code: ['kynos'],
    },
    {
        url: 'https://github.com/getkono/spargen',
        names: ['Spargen'],
        code: ['spargen'],
    },
    {
        url: 'https://github.com/visualcommons/rawshift',
        names: ['Rawshift'],
        code: ['rawshift', 'rawshift-image', 'rawshift-video'],
    },
    {
        url: 'https://github.com/visualcommons/chromahash',
        names: ['Chromahash'],
        code: ['chromahash'],
    },
    // Rust runtime, networking and TLS
    {
        url: 'https://github.com/tokio-rs/tokio',
        names: ['Tokio'],
        code: ['tokio'],
    },
    {
        url: 'https://github.com/rustls/rustls',
        names: ['rustls'],
        code: ['rustls'],
    },
    {
        url: 'https://github.com/rustls/tokio-rustls',
        names: [],
        code: ['tokio-rustls'],
    },
    { url: 'https://github.com/rustls/rcgen', names: [], code: ['rcgen'] },
    {
        url: 'https://github.com/rustls/webpki-roots',
        names: [],
        code: ['webpki-roots'],
    },
    {
        url: 'https://github.com/seanmonstar/reqwest',
        names: [],
        code: ['reqwest'],
    },
    {
        url: 'https://github.com/quinn-rs/quinn',
        names: [],
        code: ['quinn'],
    },
    { url: 'https://github.com/hyperium/h3', names: [], code: ['h3'] },
    {
        url: 'https://github.com/keepsimple1/mdns-sd',
        names: [],
        code: ['mdns-sd'],
    },
    // Cryptography and identity
    {
        url: 'https://github.com/openmls/openmls',
        names: ['OpenMLS'],
        code: ['openmls'],
    },
    {
        url: 'https://github.com/celabshq/libcrux',
        names: ['libcrux'],
        code: ['libcrux'],
    },
    {
        url: 'https://github.com/awslabs/mls-rs',
        names: [],
        code: ['mls-rs'],
    },
    {
        url: 'https://github.com/Keats/jsonwebtoken',
        names: [],
        code: ['jsonwebtoken'],
    },
    {
        url: 'https://github.com/ramosbugs/openidconnect-rs',
        names: [],
        code: ['openidconnect'],
    },
    {
        url: 'https://github.com/constantoine/totp-rs',
        names: [],
        code: ['totp-rs'],
    },
    {
        url: 'https://git.distrust.co/public/blahaj',
        names: [],
        code: ['blahaj'],
    },
    {
        url: 'https://github.com/c0dearm/sharks',
        names: [],
        code: ['sharks'],
    },
    {
        url: 'https://github.com/kanidm/webauthn-rs',
        names: [],
        code: ['webauthn-rs'],
    },
    // Storage
    {
        url: 'https://github.com/SeaQL/sea-orm',
        names: ['SeaORM'],
        code: ['sea-orm'],
    },
    { url: 'https://github.com/transact-rs/sqlx', names: [], code: ['sqlx'] },
    {
        url: 'https://github.com/rusqlite/rusqlite',
        names: [],
        code: ['rusqlite'],
    },
    {
        url: 'https://github.com/asg017/sqlite-vec',
        names: [],
        code: ['sqlite-vec'],
    },
    {
        url: 'https://github.com/postgres/postgres',
        names: ['PostgreSQL', 'Postgres'],
    },
    { url: 'https://github.com/sqlite/sqlite', names: ['SQLite'] },
    // Encoding, errors, CLI
    {
        url: 'https://github.com/enarx/ciborium',
        names: [],
        code: ['ciborium'],
    },
    { url: 'https://github.com/serde-rs/serde', names: [], code: ['serde'] },
    {
        url: 'https://github.com/BurntSushi/jiff',
        names: [],
        code: ['jiff'],
    },
    {
        url: 'https://github.com/chronotope/chrono',
        names: [],
        code: ['chrono'],
    },
    {
        url: 'https://github.com/dtolnay/thiserror',
        names: [],
        code: ['thiserror'],
    },
    {
        url: 'https://github.com/eyre-rs/eyre',
        names: [],
        code: ['eyre', 'color-eyre'],
    },
    { url: 'https://github.com/clap-rs/clap', names: [], code: ['clap'] },
    {
        url: 'https://github.com/ratatui/ratatui',
        names: ['Ratatui'],
        code: ['ratatui'],
    },
    {
        url: 'https://github.com/ratatui/ratatui-image',
        names: [],
        code: ['ratatui-image'],
    },
    {
        url: 'https://github.com/crossterm-rs/crossterm',
        names: [],
        code: ['crossterm'],
    },
    {
        url: 'https://github.com/kamadak/exif-rs',
        names: [],
        code: ['kamadak-exif'],
    },
    {
        url: 'https://github.com/evanw/thumbhash',
        names: ['ThumbHash'],
        code: ['thumbhash'],
    },
    // Bindings
    {
        url: 'https://github.com/mozilla/uniffi-rs',
        names: ['UniFFI', 'uniffi'],
        code: ['uniffi'],
    },
    {
        url: 'https://github.com/wasm-bindgen/wasm-bindgen',
        names: ['wasm-bindgen'],
        code: ['wasm-bindgen', 'wasm-bindgen-cli'],
    },
    {
        url: 'https://github.com/wasm-bindgen/wasm-pack',
        names: ['wasm-pack'],
        code: ['wasm-pack'],
    },
    {
        url: 'https://github.com/java-native-access/jna',
        names: ['JNA'],
    },
    // Testing
    {
        url: 'https://github.com/nextest-rs/nextest',
        names: ['nextest'],
        code: ['cargo-nextest'],
    },
    {
        url: 'https://github.com/testcontainers/testcontainers-rs',
        names: [],
        code: ['testcontainers'],
    },
    {
        url: 'https://github.com/proptest-rs/proptest',
        names: [],
        code: ['proptest'],
    },
    {
        url: 'https://github.com/mitsuhiko/insta',
        names: [],
        code: ['insta'],
    },
    {
        url: 'https://github.com/rust-fuzz/cargo-fuzz',
        names: [],
        code: ['cargo-fuzz'],
    },
    {
        url: 'https://github.com/sourcefrog/cargo-mutants',
        names: [],
        code: ['cargo-mutants'],
    },
    {
        url: 'https://github.com/EmbarkStudios/cargo-deny',
        names: [],
        code: ['cargo-deny'],
    },
    {
        url: 'https://github.com/robolectric/robolectric',
        names: ['Robolectric'],
    },
    {
        url: 'https://github.com/junit-team/junit-framework',
        names: ['JUnit'],
    },
    { url: 'https://github.com/mstange/samply', names: ['samply'] },
    // Tooling
    { url: 'https://github.com/jdx/mise', names: ['mise'] },
    { url: 'https://github.com/jdx/hk', names: ['hk'] },
    { url: 'https://github.com/oven-sh/bun', names: ['bun'] },
    { url: 'https://github.com/tuist/tuist', names: ['Tuist'] },
    { url: 'https://github.com/realm/SwiftLint', names: ['SwiftLint'] },
    {
        url: 'https://github.com/nicklockwood/SwiftFormat',
        names: ['SwiftFormat'],
    },
    { url: 'https://github.com/convco/convco', names: ['convco'] },
    { url: 'https://github.com/biomejs/biome', names: ['Biome'] },
    // Web and docs site
    { url: 'https://github.com/withastro/astro', names: ['Astro'] },
    { url: 'https://github.com/withastro/starlight', names: ['Starlight'] },
    {
        url: 'https://github.com/HiDeoo/starlight-blog',
        names: [],
        code: ['starlight-blog'],
    },
    {
        url: 'https://github.com/lukilabs/beautiful-mermaid',
        names: [],
        code: ['beautiful-mermaid'],
    },
    { url: 'https://github.com/vercel/geist-font', names: ['Geist'] },
    { url: 'https://github.com/react/react', names: ['React'] },
    { url: 'https://github.com/web-infra-dev/rsbuild', names: ['rsbuild'] },
    {
        url: 'https://github.com/TanStack/router',
        names: ['TanStack Router'],
    },
    { url: 'https://github.com/TanStack/query', names: ['TanStack Query'] },
    {
        url: 'https://github.com/tailwindlabs/tailwindcss',
        names: ['Tailwind'],
    },
    { url: 'https://github.com/formatjs/formatjs', names: ['FormatJS'] },
    { url: 'https://github.com/shadcn-ui/ui', names: ['shadcn'] },
    // Media
    { url: 'https://github.com/FFmpeg/FFmpeg', names: ['ffmpeg', 'FFmpeg'] },
    { url: 'https://github.com/strukturag/libheif', names: ['libheif'] },
    { url: 'https://github.com/libjxl/libjxl', names: ['libjxl'] },
    { url: 'https://github.com/libvips/libvips', names: ['libvips'] },
    { url: 'https://github.com/lovell/sharp', names: [], code: ['sharp'] },
    // Other projects the docs compare against or integrate with
    { url: 'https://github.com/immich-app/immich', names: ['Immich'] },
    { url: 'https://github.com/WeblateOrg/weblate', names: ['Weblate'] },
    {
        url: 'https://github.com/kubernetes/kubernetes',
        names: ['Kubernetes'],
    },
    { url: 'https://github.com/nginx/nginx', names: ['nginx'] },
];

/**
 * Names that are never auto-linked, and why. An author who means the library
 * writes the link.
 *
 * @type {Record<string, string>}
 */
export const EXPLICIT_ONLY = {
    gamut: 'the colour-gamut term, and visualcommons/gamut, a Rawshift dependency',
    time: 'an ordinary word, and the `time` crate',
    log: 'an ordinary word, and the `log` crate',
    image: 'an ordinary word, and the `image` crate',
    http: 'the protocol, and the `http` crate',
    bytes: 'the unit, and the `bytes` crate',
    half: 'an ordinary word, and the `half` crate',
    ring: 'an ordinary word, and the `ring` crate',
    subtle: 'an ordinary word, and the `subtle` crate',
    tracing: 'an ordinary word, and the `tracing` crate',
    uuid: 'the identifier format, and the `uuid` crate',
    sqlite: 'the database, and the `sqlite` crate',
};
