package com.justin13888.capsule.feature.albums.model

import java.time.Duration

/**
 * Which album master keys a `Welcome` hands a new member.
 *
 * Fixed per album rather than chosen per add, which is the whole point: a member's history
 * visibility must not depend on which device added them or in what order, or the same user
 * ends up able to decrypt different ranges on different devices.
 */
sealed interface HistoryPolicy {
    /** Every epoch's key travels in the `Welcome`; a joiner can read the whole album. */
    data object Full : HistoryPolicy

    /**
     * Only epochs newer than the cap travel.
     *
     * Older photos stay listed and stay undecryptable — a placeholder, never a removal.
     */
    data class Capped(
        val window: Duration,
    ) : HistoryPolicy
}

/**
 * The three values an album fixes at creation and can only change through the upgrade
 * ceremony.
 *
 * Modelled as a value the UI can only *read* — there is no setter anywhere in this feature —
 * because "changed only through an album upgrade ceremony, never ad hoc" is a rule that is
 * far easier to keep if no code path exists to break it.
 *
 * @param historyPolicy what a `Welcome` delivers to a joiner.
 * @param protocolPin the dated `protocol_version` every write into this album carries.
 * @param defaultRetention the retention window signed into this album's delete manifests.
 * @param upgradeAvailableTo the newer version this album could be forked to, or `null`
 *   when it already runs the newest one.
 */
data class AlbumPolicy(
    val historyPolicy: HistoryPolicy,
    val protocolPin: String,
    val defaultRetention: Duration,
    val upgradeAvailableTo: String? = null,
) {
    /** Whether an upgrade ceremony has anywhere to go. */
    val isUpgradable: Boolean get() = upgradeAvailableTo != null
}
