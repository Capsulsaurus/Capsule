package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.BlobHash
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.DropDescriptor
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventId
import com.justin13888.capsule.core.model.ModerationEventKind
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.ScopeId
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.ShareScope
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import java.time.Duration
import java.time.Instant

/**
 * The account the library belongs to: its owner, its devices, and the sessions on it.
 *
 * Runs before anything else, because every CRDT stamp and every OR-set add id in the library
 * names a device — a caption written by nobody is not a state the domain can represent.
 */
internal fun LibraryGenerator.synthesizeAccount() {
    val prng = accountPrng
    owner = UserId(DeterministicIds.uuidV7(spec.now.toEpochMilli(), prng))
    val enrolledAt = spec.now.minus(Duration.ofDays(ENROLLED_DAYS_AGO))
    thisDevice =
        Device(
            id = DeviceId(DeterministicIds.uuidV4(prng)),
            displayName = "Pixel 8 Pro",
            platform = PlatformTag.Android.asWire(),
            addedAt = enrolledAt,
            isThisDevice = true,
        )
    devices += thisDevice
    devices += companionDevices(prng, enrolledAt)
    synthesizeSessions(prng)
    synthesizeScopes(prng)
}

/**
 * The links, deposits and operator actions that hang off the account.
 *
 * Deferred until after the assets exist, because a share link has to point at something and a
 * link scoped to an asset id nobody generated is a dangling reference the UI would render as
 * a broken row.
 */
internal fun LibraryGenerator.synthesizeAccountArtifacts() {
    val prng = accountPrng.derive("artifacts")
    synthesizeLinks(prng)
    synthesizeDrops(prng)
    synthesizeModeration(prng)
    quota = synthesizeQuota()
}

/** The other devices on the account, including one that has been retired. */
private fun LibraryGenerator.companionDevices(
    prng: Prng,
    enrolledAt: Instant,
): List<Device> =
    listOf(
        Device(
            id = DeviceId(DeterministicIds.uuidV4(prng)),
            displayName = "MacBook Pro",
            platform = PlatformTag.Macos.asWire(),
            addedAt = enrolledAt.minus(Duration.ofDays(LAPTOP_ENROLLED_EARLIER_DAYS)),
        ),
        Device(
            id = DeviceId(DeterministicIds.uuidV4(prng)),
            displayName = "iPad Pro",
            platform = PlatformTag.Ios.asWire(),
            addedAt = enrolledAt.minus(Duration.ofDays(TABLET_ENROLLED_EARLIER_DAYS)),
        ),
        // Revoked, and deliberately still listed: every provenance record it signed is bound
        // by its key, so pruning the directory row would make the user's own history
        // unverifiable.
        Device(
            id = DeviceId(DeterministicIds.uuidV4(prng)),
            displayName = "Pixel 6",
            platform = PlatformTag.Android.asWire(),
            addedAt = enrolledAt.minus(Duration.ofDays(OLD_PHONE_ENROLLED_EARLIER_DAYS)),
            revokedAt = spec.now.minus(Duration.ofDays(OLD_PHONE_REVOKED_DAYS_AGO)),
        ),
    )

/** Sessions on the account, one of them the current process's. */
private fun LibraryGenerator.synthesizeSessions(prng: Prng) {
    val agents =
        listOf(
            "Capsule/1.0 (Android 16; Pixel 8 Pro)",
            "Capsule/1.0 (macOS 15.4; arm64)",
            "Capsule/1.0 (iPadOS 18.2)",
        )
    devices.filter(Device::isActive).forEachIndexed { index, device ->
        val createdAt = spec.now.minus(Duration.ofDays(SESSION_AGE_BASE_DAYS + index * SESSION_AGE_STRIDE_DAYS))
        val lastActive = spec.now.minus(Duration.ofHours(prng.nextInt(1, LAST_ACTIVE_HOURS_MAX).toLong()))
        val session =
            Session(
                id = SessionId(DeterministicIds.uuidV7(createdAt.toEpochMilli(), prng)),
                userId = owner,
                createdAt = createdAt,
                lastActiveAt = lastActive,
                inactivityExpiresAt = lastActive.plus(Session.DEFAULT_INACTIVITY_WINDOW),
                hardExpiresAt = createdAt.plus(Session.DEFAULT_HARD_LIFETIME),
                isCurrent = device.isThisDevice,
                deviceId = device.id,
                cohortHash = DeterministicIds.hexToken(COHORT_HASH_BYTES, prng),
                userAgent = agents.getOrElse(index) { agents.last() },
                ipAddress = "203.0.113.${prng.nextInt(2, IP_LAST_OCTET_MAX)}",
            )
        sessions += session
        cohorts +=
            DeviceCohort(
                cohortHash = session.cohortHash.orEmpty(),
                firstSeen = createdAt,
                lastSeen = lastActive,
            )
    }
    importSession = sessions.first(Session::isCurrent).id
}

/** The import sources this device has been pointed at. */
private fun LibraryGenerator.synthesizeScopes(prng: Prng) {
    // The locator is the bucket's *relative path*, never its bucket id: the id is a hash of
    // the display name and differs across devices and OS versions, so two phones pointed at
    // the same folder would otherwise compute different scope ids.
    val sources =
        listOf(
            SourceKind.CameraRoll to "DCIM/Camera",
            SourceKind.Screenshots to "Pictures/Screenshots",
            SourceKind.AppCollection to "Android/media/com.example.messenger",
            SourceKind.RemovableVolume to "SD/DCIM/100MSDCF",
        )
    sources.forEach { (kind, locator) ->
        scopes +=
            ImportScope(
                platform = PlatformTag.Android.asWire(),
                sourceKind = kind.asWire(),
                locator = locator,
                scopeId = ScopeId(DeterministicIds.hexDigest(prng)),
            )
    }
}

/** One live read link, one expired one, and one revoked one. */
private fun LibraryGenerator.synthesizeLinks(prng: Prng) {
    val visible = assets.filter { it.isInDefaultViews }
    val subject = visible.getOrNull(visible.size / SHARE_SUBJECT_DIVISOR)?.id
    val issuedAt = spec.now.minus(Duration.ofDays(SHARE_ISSUED_DAYS_AGO))
    if (subject != null) {
        shareLinks +=
            ShareLink(
                id = ShareLinkId(DeterministicIds.uuidV7(issuedAt.toEpochMilli(), prng)),
                opaqueId = DeterministicIds.hexToken(OPAQUE_ID_BYTES, prng),
                scope = ShareScope.SingleAsset(subject),
                expiresAt = spec.now.plus(Duration.ofDays(SHARE_EXPIRY_DAYS)),
                createdAt = issuedAt,
                // Present exactly once, on the link this device just issued: the server never
                // receives the fragment, so nothing can hand it back later.
                fragmentSecret = DeterministicIds.hexToken(FRAGMENT_SECRET_BYTES, prng),
            )
    }
    shareLinks +=
        ShareLink(
            id = ShareLinkId(DeterministicIds.uuidV7(issuedAt.toEpochMilli(), prng)),
            opaqueId = DeterministicIds.hexToken(OPAQUE_ID_BYTES, prng),
            scope = ShareScope.WholeAlbum(defaultAlbum.id),
            expiresAt = spec.now.minus(Duration.ofDays(SHARE_EXPIRED_DAYS_AGO)),
            createdAt = issuedAt.minus(Duration.ofDays(SHARE_EXPIRED_DAYS_AGO)),
            requiresPassphrase = true,
        )
    uploadLinks +=
        UploadLink(
            id = UploadLinkId(DeterministicIds.uuidV7(issuedAt.toEpochMilli(), prng)),
            opaqueId = DeterministicIds.hexToken(OPAQUE_ID_BYTES, prng),
            caps =
                LinkCaps(
                    expiresAt = spec.now.plus(Duration.ofDays(UPLOAD_LINK_EXPIRY_DAYS)),
                    maxTotalBytes = UPLOAD_LINK_MAX_BYTES,
                    maxFileCount = UPLOAD_LINK_MAX_FILES,
                ),
            createdAt = issuedAt,
            requiresPassphrase = true,
            bytesReceived = UPLOAD_LINK_RECEIVED_BYTES,
            filesReceived = UPLOAD_LINK_RECEIVED_FILES,
        )
}

/** Files a guest deposited that the user has not yet adopted or discarded. */
private fun LibraryGenerator.synthesizeDrops(prng: Prng) {
    val link = uploadLinks.firstOrNull() ?: return
    val names = listOf("wedding-ceremony-0431.jpg", "IMG_2210.HEIC", "reception-clip.mov")
    names.forEachIndexed { index, name ->
        val receivedAt = spec.now.minus(Duration.ofHours((index + 1) * DROP_HOURS_STRIDE))
        drops +=
            PendingDrop(
                id = DropId(DeterministicIds.uuidV7(receivedAt.toEpochMilli(), prng)),
                descriptor =
                    DropDescriptor(
                        contentType = dropContentType(name).asWire(),
                        plaintextSize = prng.nextInt(DROP_SIZE_MIN_KB, DROP_SIZE_MAX_KB).toLong() * BYTES_PER_KB,
                        ciphertextHash = BlobHash(DeterministicIds.hexDigest(prng)),
                        // Guest-supplied and unverified, and never used as a path.
                        suggestedFilename = name,
                    ),
                viaLink = link.id,
                receivedAt = receivedAt,
            )
    }
}

private fun dropContentType(name: String): ContentType =
    when {
        name.endsWith(".mov", ignoreCase = true) -> ContentType.QuickTime
        name.endsWith(".heic", ignoreCase = true) -> ContentType.Heic
        else -> ContentType.Jpeg
    }

/** One takedown and its lift, so the log shows a reversal rather than a mutation. */
private fun LibraryGenerator.synthesizeModeration(prng: Prng) {
    val subject = assets.firstOrNull { it.isInDefaultViews }?.id ?: return
    val takenDownAt = spec.now.minus(Duration.ofDays(TAKEDOWN_DAYS_AGO))
    moderationEvents +=
        ModerationEvent(
            id = ModerationEventId(DeterministicIds.uuidV7(takenDownAt.toEpochMilli(), prng)),
            userId = owner,
            assetId = subject,
            kind = ModerationEventKind.Takedown.asWire(),
            reason = "Reported by a recipient of a share link.",
            createdAt = takenDownAt,
        )
    val liftedAt = takenDownAt.plus(Duration.ofDays(TAKEDOWN_LIFTED_AFTER_DAYS))
    moderationEvents +=
        ModerationEvent(
            id = ModerationEventId(DeterministicIds.uuidV7(liftedAt.toEpochMilli(), prng)),
            userId = owner,
            assetId = subject,
            kind = ModerationEventKind.TakedownLifted.asWire(),
            reason = "Review found no policy violation.",
            createdAt = liftedAt,
        )
}

/**
 * Usage sized from the library itself rather than from a constant.
 *
 * A quota bar that disagrees with the number of photos above it is the sort of detail that
 * makes a whole mock feel wrong, so the used figure is the assets' own estimated footprint.
 */
private fun LibraryGenerator.synthesizeQuota(): QuotaStatus {
    val used = assets.sumOf { estimatedBytes(it.dimensions?.width ?: 0, it.dimensions?.height ?: 0) }
    return QuotaStatus(
        used = used,
        softLimit = (used * SOFT_LIMIT_HEADROOM).toLong(),
        hardLimit = (used * HARD_LIMIT_HEADROOM).toLong(),
    )
}

/** A rough compressed size for a frame of these dimensions, in bytes. */
private fun estimatedBytes(
    width: Int,
    height: Int,
): Long = (width.toLong() * height.toLong() * BYTES_PER_PIXEL_NUMERATOR) / BYTES_PER_PIXEL_DENOMINATOR

private const val ENROLLED_DAYS_AGO = 420L
private const val LAPTOP_ENROLLED_EARLIER_DAYS = 200L
private const val TABLET_ENROLLED_EARLIER_DAYS = 90L
private const val OLD_PHONE_ENROLLED_EARLIER_DAYS = 900L
private const val OLD_PHONE_REVOKED_DAYS_AGO = 380L
private const val SESSION_AGE_BASE_DAYS = 12L
private const val SESSION_AGE_STRIDE_DAYS = 31L
private const val LAST_ACTIVE_HOURS_MAX = 70
private const val COHORT_HASH_BYTES = 32
private const val IP_LAST_OCTET_MAX = 250
private const val OPAQUE_ID_BYTES = 16
private const val FRAGMENT_SECRET_BYTES = 16
private const val SHARE_SUBJECT_DIVISOR = 7
private const val SHARE_ISSUED_DAYS_AGO = 9L
private const val SHARE_EXPIRY_DAYS = 21L
private const val SHARE_EXPIRED_DAYS_AGO = 40L
private const val UPLOAD_LINK_EXPIRY_DAYS = 30L
private const val UPLOAD_LINK_MAX_BYTES = 8L * 1024 * 1024 * 1024
private const val UPLOAD_LINK_MAX_FILES = 200
private const val UPLOAD_LINK_RECEIVED_BYTES = 412L * 1024 * 1024
private const val UPLOAD_LINK_RECEIVED_FILES = 3
private const val DROP_HOURS_STRIDE = 5L
private const val DROP_SIZE_MIN_KB = 900
private const val DROP_SIZE_MAX_KB = 9_000
private const val BYTES_PER_KB = 1024L
private const val TAKEDOWN_DAYS_AGO = 64L
private const val TAKEDOWN_LIFTED_AFTER_DAYS = 3L
private const val SOFT_LIMIT_HEADROOM = 1.18
private const val HARD_LIMIT_HEADROOM = 1.45
private const val BYTES_PER_PIXEL_NUMERATOR = 3L
private const val BYTES_PER_PIXEL_DENOMINATOR = 8L
