import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Membership

/// Membership changes are **MLS commits, not settings writes**.
///
/// Every method here bumps the AMK epoch, because on the far side each one is a
/// group operation that rekeys the album. That is why the UI must never let a
/// role look changed before the commit lands, and why these are `async throws`
/// rather than a bound toggle: they take real time and can genuinely fail
/// partway.
public extension MockLibraryStore {
    /// Invite a user by handle, at a role.
    ///
    /// Issues an `Add` for every one of their devices and bumps the epoch. A
    /// handle already in the album is a no-op rather than a duplicate row — the
    /// same idempotence a move has, for the same reason.
    func inviteMember(handle: String, role: AlbumRole, to albumID: AlbumID) async throws {
        try role.requireWritable()
        try requireAdministrable(albumID)
        updateContainer(albumID) { album in
            guard !album.members.contains(where: { $0.handle == handle }) else { return }
            album.members.append(AlbumMember(handle: handle, role: role))
            album.epoch += 1
        }
        await albumChanges.send(())
    }

    /// Change a member's role — a commit, and another epoch.
    func setMemberRole(_ role: AlbumRole, for handle: String, in albumID: AlbumID) async throws {
        try role.requireWritable()
        try requireAdministrable(albumID)
        var found = false
        updateContainer(albumID) { album in
            guard let position = album.members.firstIndex(where: { $0.handle == handle }) else { return }
            album.members[position].role = role
            album.epoch += 1
            found = true
        }
        guard found else {
            throw CapsuleError(code: .albumInvalidID, detail: "CapsuleMock: no such member")
        }
        await albumChanges.send(())
    }

    /// Remove a member — a `Remove` plus an epoch bump, so they derive no future
    /// key. Historical epochs they already hold are not, and cannot be,
    /// retracted; a UI that implies otherwise is promising something MLS does
    /// not.
    func removeMember(handle: String, from albumID: AlbumID) async throws {
        try requireAdministrable(albumID)
        updateContainer(albumID) { album in
            let before = album.members.count
            album.members.removeAll { $0.handle == handle }
            if album.members.count != before { album.epoch += 1 }
        }
        await albumChanges.send(())
    }

    /// Refuse a membership change on an album this user cannot administer.
    ///
    /// The capability is cryptographic — admin holds the admin-tier key — so
    /// this is a real refusal, not a disabled button.
    private func requireAdministrable(_ albumID: AlbumID) throws {
        guard let album = container(albumID) else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: unknown album")
        }
        let owner = album.members.first { $0.handle == MockSidecarFactory.ownerHandle }
        guard album.members.isEmpty || owner?.role.canAdminister == true else {
            throw CapsuleError(
                code: .uploadOwnerNotPermitted,
                detail: "CapsuleMock: this account does not hold the album's admin-tier key"
            )
        }
    }
}
