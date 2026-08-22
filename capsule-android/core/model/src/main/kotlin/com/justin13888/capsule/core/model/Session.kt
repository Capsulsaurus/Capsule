package com.justin13888.capsule.core.model

import java.time.Duration
import java.time.Instant

/**
 * The platform a device runs on — a closed enum shared by device cohorts and import scopes.
 *
 * Shared deliberately: a scope's identity is `(platform, source_kind, locator)`, and the
 * locator's canonical form differs per platform, so the two surfaces must agree on the
 * platform vocabulary or two devices would compute different scope ids for the same folder.
 */
enum class PlatformTag(
    override val wire: String,
) : WireEnum {
    /** iOS and iPadOS. */
    Ios("ios"),

    /** Android. */
    Android("android"),

    /** macOS. */
    Macos("macos"),

    /** Windows. */
    Windows("windows"),

    /** Linux. */
    Linux("linux"),
    ;

    companion object : WireCodec<PlatformTag>({ entries })
}

/**
 * A grouping of sessions that came from the same physical device.
 *
 * The hash is computed from a platform-specific primary identifier and is **client-asserted
 * and unverifiable**, which is the single most important thing about it: no authorization or
 * capability decision may read it. It exists so a session list can say "these three sessions
 * are the same phone" — a support and comprehension aid, never a security control.
 */
data class DeviceCohort(
    /** `SHA-256` over domain-separated canonical CBOR of the cohort inputs. */
    val cohortHash: String,
    /** When a session from this cohort was first seen. */
    val firstSeen: Instant,
    /** When one was last seen. */
    val lastSeen: Instant,
)

/**
 * An authenticated session, as the session-management surface lists it.
 *
 * Two expiries run at once and either one ends the session, which is why both are fields
 * rather than one derived deadline. [inactivityExpiresAt] slides forward on every successful
 * access-token issuance; [hardExpiresAt] does not move for any reason. Modelling only the
 * sliding one would let a session that is used daily live forever.
 */
data class Session(
    /** The session's own id — UUIDv7, minted by the server. Not a device id. */
    val id: SessionId,
    /** The account. */
    val userId: UserId,
    /** When the session was established. */
    val createdAt: Instant,
    /** The last successful access-token issuance against it. */
    val lastActiveAt: Instant,
    /** The sliding deadline, refreshed on every use. */
    val inactivityExpiresAt: Instant,
    /** The upper bound on lifetime regardless of activity. Never moves. */
    val hardExpiresAt: Instant,
    /** Whether this is the session the current process is using. */
    val isCurrent: Boolean = false,
    /** The enrolled device, when the session is bound to one. */
    val deviceId: DeviceId? = null,
    /** Which physical device this session appears to come from. Never authorization input. */
    val cohortHash: String? = null,
    /** The client string the server recorded. */
    val userAgent: String? = null,
    /** The address the server recorded. */
    val ipAddress: String? = null,
    /** Set when the user explicitly revoked it. */
    val revokedAt: Instant? = null,
) {
    /**
     * Whether the session would still be honoured at [now].
     *
     * Fail-closed by construction: any of revocation, inactivity, or the hard bound ends it,
     * and a caller cannot accidentally check only one.
     */
    fun isActiveAt(now: Instant): Boolean = revokedAt == null && now.isBefore(effectiveExpiry)

    /** Whichever deadline arrives first — what a UI should count down to. */
    val effectiveExpiry: Instant
        get() = if (inactivityExpiresAt.isBefore(hardExpiresAt)) inactivityExpiresAt else hardExpiresAt

    companion object {
        /** Default sliding inactivity window; deployment-configurable. */
        val DEFAULT_INACTIVITY_WINDOW: Duration = Duration.ofDays(180)

        /** Default hard lifetime; deployment-configurable. Does not reset on use. */
        val DEFAULT_HARD_LIFETIME: Duration = Duration.ofDays(365)
    }
}

/**
 * An enrolled device, as the signed device directory records it.
 *
 * [revokedAt] is a timestamp rather than a state flag because a revoked device's record is
 * **never pruned**: every provenance record it ever signed is bound by its key, so removing
 * the public half would make the user's own history unverifiable. Revocation stops future
 * writes; it does not erase past ones.
 */
data class Device(
    /** The device's id — a security-bearing random UUIDv4. */
    val id: DeviceId,
    /** A user-chosen label, when one has been set. */
    val displayName: String?,
    /** What the device runs. */
    val platform: Wire<PlatformTag>,
    /** When the directory first carried it. Also a validation input on inbound manifests. */
    val addedAt: Instant,
    /** When it was retired. Never cleared, and the record is never deleted. */
    val revokedAt: Instant? = null,
    /** Whether this is the device the current process is running on. */
    val isThisDevice: Boolean = false,
) {
    /** Whether the device may still sign new writes. */
    val isActive: Boolean get() = revokedAt == null
}

/**
 * A view that must not open on a borrowed unlocked phone.
 *
 * The gate is view-time UX protection, explicitly **not** a cryptographic boundary — the
 * same bytes are reachable by anyone who defeats the platform sandbox. Saying so in the type
 * documentation matters because a reader who assumed otherwise might skip real at-rest
 * protection on the strength of it.
 */
enum class GatedView(
    override val wire: String,
) : WireEnum {
    /** The trash listing. */
    RecentlyDeleted("recently_deleted"),

    /** The dedicated view for assets the user hid. */
    Hidden("hidden"),
    ;

    companion object : WireCodec<GatedView>({ entries })
}

/**
 * A grant of fresh local authentication for one [GatedView].
 *
 * Grants are **per view**, not global: unlocking Recently Deleted must not also unlock
 * Hidden, because the two answer different questions about what the user meant to keep out
 * of sight.
 */
data class LocalAuthGrant(
    /** Which view this grant opens. */
    val view: GatedView,
    /** When the user authenticated. */
    val grantedAt: Instant,
    /** How long the grant lasts. */
    val window: Duration = DEFAULT_WINDOW,
) {
    /** Whether the view may open at [now] without re-authenticating. */
    fun isFreshAt(now: Instant): Boolean = now.isBefore(grantedAt.plus(window))

    companion object {
        /** The default grace window, per view. */
        val DEFAULT_WINDOW: Duration = Duration.ofMinutes(5)
    }
}
