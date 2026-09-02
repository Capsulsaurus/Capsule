import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Stacking

/// Stack derivation.
///
/// Stacks are the reason the mock has more than one asset population. A
/// collapsed stack shows only its primary, so its other members are **not in
/// the default timeline** — and rather than deriving them into the base index
/// and filtering them out (which would cost an O(assets) scan on every
/// aggregate), they are derived on demand as ``MockAssetRef/Kind/stackMember``
/// off their primary's index. The base index therefore *is* the default
/// timeline, exactly.
public extension MockLibrary {
    /// Roughly one asset in fourteen leads a stack.
    private static var stackingStride: Int { 14 }

    /// The kind of stack an asset leads, or `nil` when it is not stacked.
    ///
    /// A RAW file always pairs with its JPEG, because that is what a camera
    /// writing RAW+JPEG produces; everything else is drawn from the types a
    /// phone and a mirrorless body actually generate.
    func stackType(at index: Int) -> StackType? {
        guard index >= 0, index < assetCount else { return nil }
        let hash = MockHash.value(seed: profile.seed, index: index, salt: .stacking)
        guard Int(hash % UInt64(Self.stackingStride)) == 0 else { return nil }
        let ref = MockAssetRef(kind: .live, index: index)
        if contentType(for: ref) == .dng { return .rawJpeg }
        let choices: [StackType] = [.burst, .livePhoto, .portrait, .hdrBracket, .panorama]
        return MockHash.element(MockHash.mix(hash), from: choices) ?? .burst
    }

    /// How many assets the stack at this index holds, the primary included.
    /// `1` for an unstacked asset, so callers need no special case.
    func stackMemberCount(at index: Int) -> Int {
        guard let type = stackType(at: index) else { return 1 }
        switch type {
        case .rawJpeg, .livePhoto, .portrait:
            return 2
        case .burst:
            let hash = MockHash.value(seed: profile.seed, index: index, salt: .stacking, sub: 3)
            return MockHash.integer(hash, in: 3 ... 8)
        default:
            let hash = MockHash.value(seed: profile.seed, index: index, salt: .stacking, sub: 5)
            return MockHash.integer(hash, in: 2 ... 5)
        }
    }

    /// The stack's identity. Derived from the primary's index, so a stack id is
    /// as stable as the asset that leads it.
    func stackIdentifier(at index: Int) -> StackID {
        MockIdentifiers.stackID(seed: profile.seed, ordinal: index)
    }

    /// One asset's membership register.
    ///
    /// A member's ``StackRole`` is not cosmetic: the JPEG half of a RAW+JPEG
    /// pair and a Live Photo's clip are suppressed from default views by their
    /// *role*, which is why neither needs a hidden flag.
    func stackMembership(for ref: MockAssetRef) -> StackMembership? {
        switch ref.kind {
        case .live:
            guard let type = stackType(at: ref.index) else { return nil }
            return StackMembership(
                stackID: stackIdentifier(at: ref.index),
                stackType: type,
                role: .primary,
                memberIndex: 0
            )
        case .stackMember:
            guard let type = stackType(at: ref.index) else { return nil }
            return StackMembership(
                stackID: stackIdentifier(at: ref.index),
                stackType: type,
                role: memberRole(type: type, ordinal: ref.memberOrdinal),
                memberIndex: UInt32(ref.memberOrdinal)
            )
        case .trashed, .userHidden:
            return nil
        }
    }

    /// The role a non-primary member takes. A video master's lightweight
    /// companion is a ``StackRole/proxy``; everything else is an ordinary
    /// member.
    private func memberRole(type: StackType, ordinal: UInt8) -> StackRole {
        type == .proxy && ordinal == 1 ? .proxy : .member
    }

    /// Every reference in a stack, primary first, in `member_index` order.
    func stackRefs(at index: Int) -> [MockAssetRef] {
        let total = stackMemberCount(at: index)
        guard total > 1 else { return [MockAssetRef(kind: .live, index: index)] }
        var refs = [MockAssetRef(kind: .live, index: index)]
        for ordinal in 1 ..< total {
            refs.append(MockAssetRef(kind: .stackMember, index: index, memberOrdinal: UInt8(ordinal)))
        }
        return refs
    }

    /// The stack as the UI presents it, with its **derived** group cull state.
    ///
    /// Derived, never stored: a stored group flag would be a second source of
    /// truth that diverges the moment one member is re-flagged.
    func stack(at index: Int) -> Stack? {
        guard let type = stackType(at: index) else { return nil }
        let refs = stackRefs(at: index)
        return Stack(
            id: stackIdentifier(at: index),
            stackType: type,
            primaryAssetID: refs[0].uuidString(seed: profile.seed),
            memberAssetIDs: refs.map { $0.uuidString(seed: profile.seed) },
            cullState: GroupCullState(members: refs.map { cullFlag(for: $0) })
        )
    }

    /// Resolve a stack identifier back to the index that leads it.
    ///
    /// A scan, but bounded: stack ids are minted from the primary's index, so
    /// the search space is the library and the loop stops at the first match.
    /// The alternative — a stored map — is the materialization this whole module
    /// exists to avoid.
    func stackIndex(of identifier: StackID) -> Int? {
        for index in stride(from: 0, to: assetCount, by: Self.stackingStride) {
            for candidate in index ..< min(index + Self.stackingStride, assetCount)
                where stackType(at: candidate) != nil {
                if stackIdentifier(at: candidate) == identifier { return candidate }
            }
        }
        return nil
    }

    /// The presentation media type.
    ///
    /// A Live Photo is **not** derivable from the content type — it is a still
    /// stacked with a clip — so the stack type decides it, exactly as the domain
    /// documents.
    func mediaType(for ref: MockAssetRef, contentType: ContentType) -> MediaType {
        if case .live = ref.kind, stackType(at: ref.index) == .livePhoto {
            return .livePhoto
        }
        return contentType.presentationMediaType
    }
}
