import Foundation

/// Encodes and decodes the sidecar that is written beside every managed media
/// file.
///
/// The *canonical* sidecar is the signed CBOR `SidecarV1`, authored only by
/// the Rust core; a build that links the core has no sidecar codec of its own
/// (the unsigned CBOR codec was retired with `S-D24`). `JSONSidecarCoder` is
/// the codec of the mock lane. This is a protocol rather than an `enum` of
/// statics so the coder is injectable:
/// the mock lane builds and tests without linking the Rust core, and a failure
/// mode (a sidecar the core rejects) can be simulated deterministically.
///
/// Whatever the implementation, one invariant is absolute: fields written by a
/// newer build must survive a round trip via ``CatalogSidecar/unknownFieldsCBOR``.
/// A coder that drops them is buggy by definition — stripping unknown sidecar
/// fields is a forbidden client behaviour.
public protocol SidecarCoding: Sendable {
    /// Encode a sidecar to bytes.
    ///
    /// - Throws: ``CatalogError/sidecar(message:)`` if a type-like field holds a
    ///   value the format does not recognise.
    func encode(_ sidecar: CatalogSidecar) throws -> Data

    /// Decode bytes into a sidecar.
    ///
    /// - Throws: ``CatalogError/sidecar(message:)`` if `data` is not a valid sidecar.
    func decode(_ data: Data) throws -> CatalogSidecar
}

/// A JSON-backed ``SidecarCoding`` for builds that do not link the Rust core.
///
/// This is **not** the canonical wire format and never writes a file another
/// Capsule client will read — it exists so the mock lane, previews, and unit
/// tests have a real, round-tripping coder instead of a stub. It preserves
/// ``CatalogSidecar/unknownFieldsCBOR`` verbatim, so the preservation invariant
/// above is exercised here too.
public struct JSONSidecarCoder: SidecarCoding {
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init() {
        encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        decoder = JSONDecoder()
    }

    public func encode(_ sidecar: CatalogSidecar) throws -> Data {
        do {
            return try encoder.encode(sidecar)
        } catch {
            throw CatalogError.sidecar(message: "json encode failed: \(error)")
        }
    }

    public func decode(_ data: Data) throws -> CatalogSidecar {
        do {
            return try decoder.decode(CatalogSidecar.self, from: data)
        } catch {
            throw CatalogError.sidecar(message: "json decode failed: \(error)")
        }
    }
}
