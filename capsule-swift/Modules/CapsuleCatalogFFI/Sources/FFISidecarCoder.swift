import CapsuleCatalog
import Foundation

/// The canonical ``SidecarCoding`` — canonical CBOR, produced by the Rust core.
///
/// A thin, stateless façade over the uniffi `serialize_sidecar` /
/// `deserialize_sidecar` functions. It is a pure transform: no I/O, no shared
/// state, so it is safe to share freely.
///
/// This is the only coder whose output another Capsule client will read.
/// `JSONSidecarCoder` exists for the mock lane; this one is the wire format.
public struct FFISidecarCoder: SidecarCoding {
    public init() {}

    public func encode(_ sidecar: CatalogSidecar) throws -> Data {
        do {
            return try serializeSidecar(record: sidecar.ffiRecord)
        } catch {
            throw nativeCatalogError(error)
        }
    }

    public func decode(_ data: Data) throws -> CatalogSidecar {
        do {
            return try CatalogSidecar(deserializeSidecar(bytes: data))
        } catch {
            throw nativeCatalogError(error)
        }
    }
}
