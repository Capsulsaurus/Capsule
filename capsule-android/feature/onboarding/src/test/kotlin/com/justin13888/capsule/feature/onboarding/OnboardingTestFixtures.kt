package com.justin13888.capsule.feature.onboarding

import kotlinx.collections.immutable.toImmutableList

/** A server that offers every path, so a test can pick whichever one it means to exercise. */
val FIXTURE_SERVER: ServerInfo =
    ServerInfo(
        apiBase = "https://photos.example.org/api/v1",
        authMethods = AuthMethod.entries.toImmutableList(),
        minProtocolVersion = 1,
        maxProtocolVersion = 3,
    )

/**
 * A [MediaAccessProbe] a test moves by hand.
 *
 * Hand-rolled rather than driven from `core:mock`, which has no media-permission fake: the
 * platform's answer is not library state, so nothing in the seeded backend could stand in
 * for it.
 */
class StubMediaAccessProbe(
    /** What the next [current] call reports. */
    var access: MediaAccess = MediaAccess.None,
) : MediaAccessProbe {
    override fun current(): MediaAccess = access

    override fun requestedPermissions(): List<String> = listOf("android.permission.READ_MEDIA_IMAGES")
}
