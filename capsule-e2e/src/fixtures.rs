//! Byte-built fixtures, so the repository carries no binary test asset, and the committed
//! golden wire fixtures ([`golden`]), which are text for the same reason.

/// A real 8×8 grayscale baseline JPEG carrying an EXIF APP1 segment, built byte by byte.
///
/// The same construction as the CLI's import round trip
/// (`capsule-cli/tests/import_round_trip.rs`), which a test crate cannot import. An 8×8 still
/// sits inside the thumbnail tier's 256-pixel cap, so the media stack signs the byte-free
/// `original` sentinel for it and the upload bundle carries no derivative bytes — the cheap
/// fixture for every case that is not about derivatives; [`large_synthetic_jpeg`] is the one
/// that produces a real thumbnail.
#[must_use]
pub fn synthetic_jpeg() -> Vec<u8> {
    synthetic_jpeg_sized(8, 8)
}

/// A 512×512 still of the same construction: past the thumbnail tier's long-edge cap, so the
/// media stack decodes it and encodes a real JXL thumbnail for the upload ladder's T1 — the
/// still E2E case 2 pushes.
#[must_use]
pub fn large_synthetic_jpeg() -> Vec<u8> {
    synthetic_jpeg_sized(512, 512)
}

/// A grayscale baseline JPEG of `width`×`height` (each a multiple of 8) carrying an EXIF APP1
/// segment, built byte by byte.
///
/// The EXIF block is a big-endian TIFF structure with three IFDs — IFD0 (make/model +
/// pointers), the Exif SubIFD (`DateTimeOriginal`, `OffsetTimeOriginal`, pixel dimensions) and
/// the GPS IFD — and the image is a genuine baseline JPEG a conformant decoder accepts: every
/// 8×8 block is the shortest legal encoding of an all-zero block (DC category 0, then EOB), two
/// bits each, so the picture is a flat mid-grey.
#[must_use]
pub fn synthetic_jpeg_sized(width: u16, height: u16) -> Vec<u8> {
    assert!(
        width.is_multiple_of(8) && height.is_multiple_of(8) && width > 0 && height > 0,
        "dimensions are whole 8×8 blocks"
    );
    const ASCII: u16 = 2;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    const MAKE: &[u8] = b"Capsule\0";
    const MODEL: &[u8] = b"Synth\0";
    const DATE_TIME_ORIGINAL: &[u8] = b"2019:03:04 05:06:07\0";
    const OFFSET_TIME_ORIGINAL: &[u8] = b"+00:00\0";

    // Each IFD here holds four entries: 2 count bytes + 4×12 entry bytes + 4 next-IFD bytes.
    const IFD_LEN: u32 = 2 + 4 * 12 + 4;
    const IFD0_AT: u32 = 8;
    const EXIF_IFD_AT: u32 = IFD0_AT + IFD_LEN;
    const GPS_IFD_AT: u32 = EXIF_IFD_AT + IFD_LEN;
    const DATA_AT: u32 = GPS_IFD_AT + IFD_LEN;
    const MAKE_AT: u32 = DATA_AT;
    const MODEL_AT: u32 = MAKE_AT + MAKE.len() as u32;
    const DTO_AT: u32 = MODEL_AT + MODEL.len() as u32;
    const OTO_AT: u32 = DTO_AT + DATE_TIME_ORIGINAL.len() as u32;
    // Rationals are 4-byte quantities; one pad byte keeps them aligned.
    const LAT_AT: u32 = OTO_AT + OFFSET_TIME_ORIGINAL.len() as u32 + 1;
    const LON_AT: u32 = LAT_AT + 24;

    /// One 12-byte IFD entry whose value is an offset into the TIFF block.
    fn at(tag: u16, kind: u16, count: u32, offset: u32) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&offset.to_be_bytes());
        e
    }

    /// One 12-byte IFD entry whose value fits in the 4 inline bytes.
    fn inline(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&value);
        e
    }

    fn rational(numerator: u32, denominator: u32) -> Vec<u8> {
        let mut r = Vec::with_capacity(8);
        r.extend_from_slice(&numerator.to_be_bytes());
        r.extend_from_slice(&denominator.to_be_bytes());
        r
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&IFD0_AT.to_be_bytes());

    // IFD0: Make, Model, and the pointers to the two sub-IFDs.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x010F, ASCII, MAKE.len() as u32, MAKE_AT));
    tiff.extend(at(0x0110, ASCII, MODEL.len() as u32, MODEL_AT));
    tiff.extend(at(0x8769, LONG, 1, EXIF_IFD_AT));
    tiff.extend(at(0x8825, LONG, 1, GPS_IFD_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // Exif SubIFD: capture time, its UTC offset, and the pixel dimensions.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x9003, ASCII, DATE_TIME_ORIGINAL.len() as u32, DTO_AT));
    tiff.extend(at(0x9011, ASCII, OFFSET_TIME_ORIGINAL.len() as u32, OTO_AT));
    tiff.extend(inline(0xA002, LONG, 1, u32::from(width).to_be_bytes()));
    tiff.extend(inline(0xA003, LONG, 1, u32::from(height).to_be_bytes()));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // GPS IFD: 48°51'29.6"N, 2°17'40.2"W.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(inline(0x0001, ASCII, 2, *b"N\0\0\0"));
    tiff.extend(at(0x0002, RATIONAL, 3, LAT_AT));
    tiff.extend(inline(0x0003, ASCII, 2, *b"W\0\0\0"));
    tiff.extend(at(0x0004, RATIONAL, 3, LON_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // The out-of-line values, in the order the offsets above declare.
    tiff.extend_from_slice(MAKE);
    tiff.extend_from_slice(MODEL);
    tiff.extend_from_slice(DATE_TIME_ORIGINAL);
    tiff.extend_from_slice(OFFSET_TIME_ORIGINAL);
    tiff.push(0);
    for (numerator, denominator) in [(48, 1), (51, 1), (296, 10), (2, 1), (17, 1), (402, 10)] {
        tiff.extend(rational(numerator, denominator));
    }
    assert_eq!(
        tiff.len() as u32,
        LON_AT + 24,
        "the TIFF block must be exactly as long as its own offsets claim"
    );

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);

    let mut jpeg = vec![0xFF, 0xD8]; // SOI
    jpeg.extend_from_slice(&[0xFF, 0xE1]); // APP1
    jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(&app1);

    // DQT: one flat 8-bit luminance table.
    jpeg.extend_from_slice(&[0xFF, 0xDB]);
    jpeg.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    jpeg.push(0x00);
    jpeg.extend(std::iter::repeat_n(1u8, 64));

    // SOF0: baseline, 8-bit, one component with no subsampling.
    jpeg.extend_from_slice(&[0xFF, 0xC0]);
    jpeg.extend_from_slice(&11u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x08]);
    jpeg.extend_from_slice(&height.to_be_bytes());
    jpeg.extend_from_slice(&width.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);

    // DHT: a DC and an AC table each holding a single 1-bit code for symbol 0.
    for class_and_id in [0x00u8, 0x10] {
        jpeg.extend_from_slice(&[0xFF, 0xC4]);
        jpeg.extend_from_slice(&(2u16 + 1 + 16 + 1).to_be_bytes());
        jpeg.push(class_and_id);
        jpeg.push(1);
        jpeg.extend(std::iter::repeat_n(0u8, 15));
        jpeg.push(0x00);
    }

    // SOS, then the entropy-coded data: two zero bits per block (DC category 0, then EOB),
    // the last partial byte padded with 1 bits as the standard requires.
    jpeg.extend_from_slice(&[0xFF, 0xDA]);
    jpeg.extend_from_slice(&8u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    let blocks = usize::from(width / 8) * usize::from(height / 8);
    let bits = blocks * 2;
    jpeg.extend(std::iter::repeat_n(0u8, bits / 8));
    let dangling = bits % 8;
    if dangling != 0 {
        jpeg.push((1u8 << (8 - dangling)) - 1);
    }

    jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
    jpeg
}

/// Golden wire fixtures, one directory per protocol version ([Versioning — Validation],
/// invariants 2, 3 and 9).
///
/// `capsule-e2e/tests/fixtures/golden/<protocol_version>/` holds what a build speaking that
/// protocol version put on the wire, recorded once from the real server composition root and
/// the real library by `tests/golden_wire.rs`'s recorder:
///
/// - [`golden::TRANSCRIPT_FILE`]: a [`golden::Transcript`] — every request and response of one
///   scripted session, in order, as JSON;
/// - [`golden::RECORDS_FILE`]: [`golden::Records`] — signed records as canonical CBOR, hex.
///
/// Every later build must still read every directory: the server must answer each replayable
/// request as it did, the generated client must decode each response, and core must decode,
/// verify and re-emit each record byte for byte. A directory is never edited once a release
/// has spoken its version; a new protocol version gets a new directory beside it.
///
/// Text rather than binary for the reason the JPEG above is built byte by byte: the repository
/// carries no binary test asset, and a fixture a reviewer can read in a diff is one whose
/// change a reviewer notices.
///
/// [Versioning — Validation]: https://docs/design/versioning/#validation
pub mod golden {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    /// The recorded session, inside a version directory.
    pub const TRANSCRIPT_FILE: &str = "transcript.json";

    /// The recorded signed records, inside a version directory.
    pub const RECORDS_FILE: &str = "records.json";

    /// What every bearer and refresh token is replaced with before a transcript is written. A
    /// token is a credential whatever server minted it, and its value is no part of the wire
    /// contract: a client reads it as an opaque string, which the placeholder still is.
    pub const REDACTED: &str = "<redacted>";

    /// One request and the response it drew.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Exchange {
        /// A stable name for the step, unique within its transcript.
        pub name: String,
        /// The HTTP method.
        pub method: String,
        /// The path and query, relative to the API root.
        pub path: String,
        /// The `X-Capsule-Protocol` the request carried, where it was not the transcript's own
        /// version (a request recorded from outside the server's window).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub protocol: Option<String>,
        /// Whether the request carried the session's bearer token.
        pub authenticated: bool,
        /// Whether a later server can be handed this request on a fresh account and must answer
        /// it with the same status. `false` for a step that reads state the recording built
        /// through the SDK (a pushed asset), whose ids a replay cannot reproduce.
        pub replay: bool,
        /// The JSON request body, if the request had one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub request: Option<Value>,
        /// The response status.
        pub status: u16,
        /// The OpenAPI component the response body is an instance of.
        pub schema: String,
        /// The JSON response body, tokens [redacted](REDACTED).
        pub response: Value,
    }

    /// One recorded session against the server, in order.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Transcript {
        /// The protocol version every request carried unless its [`Exchange::protocol`] says
        /// otherwise; the same as the directory's name.
        pub protocol_version: String,
        /// The exchanges, in the order they were made.
        pub exchanges: Vec<Exchange>,
    }

    impl Transcript {
        /// The exchange named `name`.
        #[must_use]
        pub fn exchange(&self, name: &str) -> Option<&Exchange> {
            self.exchanges.iter().find(|e| e.name == name)
        }
    }

    /// Signed records as a build of this version wrote them: canonical CBOR, lowercase hex.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Records {
        /// The account's device directory, exactly as `GET /v1/auth/devices/directory/{id}`
        /// served it.
        pub device_directory: String,
        /// The user identity key the directory is signed under (`classical ‖ ML-DSA-65`).
        pub user_ik_public: String,
        /// The head provenance record of the asset the recording pushed: a `create` whose
        /// manifest is signed by a device the directory lists.
        pub provenance_record: String,
    }

    /// `capsule-e2e/tests/fixtures/golden`.
    #[must_use]
    pub fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
    }

    /// Every recorded protocol version and its directory, oldest first.
    #[must_use]
    pub fn versions() -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(root())
            .expect("the golden fixture root exists")
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|path| path.is_dir())
            .map(|path| {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .expect("a UTF-8 directory name")
                    .to_owned();
                (name, path)
            })
            .collect();
        found.sort();
        found
    }

    /// Read `dir`'s transcript.
    #[must_use]
    pub fn load_transcript(dir: &Path) -> Transcript {
        read_json(&dir.join(TRANSCRIPT_FILE))
    }

    /// Read `dir`'s records.
    #[must_use]
    pub fn load_records(dir: &Path) -> Records {
        read_json(&dir.join(RECORDS_FILE))
    }

    fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|e| panic!("golden fixture {} reads: {e}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("golden fixture {} parses: {e}", path.display()))
    }

    /// Write `value` to `path` as pretty JSON with a trailing newline, object keys sorted, so a
    /// re-recording diffs line by line.
    pub fn write_json<T: Serialize>(path: &Path, value: &T) {
        let mut text = serde_json::to_string_pretty(value).expect("a fixture serializes");
        text.push('\n');
        std::fs::write(path, text)
            .unwrap_or_else(|e| panic!("golden fixture {} writes: {e}", path.display()));
    }

    /// Replace the value of every `access_token` and `refresh_token` key, at any depth, with
    /// [`REDACTED`].
    pub fn redact_tokens(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map.iter_mut() {
                    if key == "access_token" || key == "refresh_token" {
                        *inner = Value::String(REDACTED.to_owned());
                    } else {
                        redact_tokens(inner);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(redact_tokens),
            _ => {}
        }
    }

    /// Lowercase hex.
    #[must_use]
    pub fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
                let _ = write!(out, "{b:02x}");
                out
            })
    }

    /// The bytes `hex` spells. Panics on anything but an even run of hex digits: a fixture that
    /// does not parse is a broken fixture, not an input to tolerate.
    #[must_use]
    pub fn from_hex(hex: &str) -> Vec<u8> {
        assert!(hex.len().is_multiple_of(2), "an even number of hex digits");
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digits"))
            .collect()
    }
}
