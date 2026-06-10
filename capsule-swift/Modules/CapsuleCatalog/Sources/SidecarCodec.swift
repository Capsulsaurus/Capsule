import Foundation

/// Encodes and decodes the canonical CBOR sidecar format.
///
/// The format is owned by the Rust core; this is a thin, stateless façade over
/// the UniFFI `serialize_sidecar` / `deserialize_sidecar` functions. It is a
/// pure transform — no I/O, no shared state — so it is a plain `enum` of
/// `static` functions rather than a protocol: there is nothing to mock.
///
/// `ManagedStore` uses it to turn a ``CatalogSidecar`` into bytes to write
/// beside a media file, and to read those bytes back. Fields written by a
/// newer build survive a round trip via ``CatalogSidecar/unknownFieldsCBOR``.
public enum SidecarCodec {
    /// Encode a sidecar to canonical CBOR bytes.
    ///
    /// - Throws: `CatalogError.Sidecar` if a type-like field holds a value the
    ///   core does not recognise.
    public static func encode(_ sidecar: CatalogSidecar) throws -> Data {
        try serializeSidecar(record: sidecar.ffiRecord)
    }

    /// Decode canonical CBOR bytes into a sidecar.
    ///
    /// - Throws: `CatalogError.Sidecar` if `data` is not a valid sidecar.
    public static func decode(_ data: Data) throws -> CatalogSidecar {
        CatalogSidecar(try deserializeSidecar(bytes: data))
    }
}
