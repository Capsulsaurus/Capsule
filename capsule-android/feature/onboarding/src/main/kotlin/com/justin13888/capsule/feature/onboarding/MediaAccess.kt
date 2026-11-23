package com.justin13888.capsule.feature.onboarding

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat

/**
 * How much of the device's photo library this app may currently read.
 *
 * [Partial] is a real, supported state rather than a failure to reach [Full]. From Android 14
 * the system offers "select photos" as a first-class answer, and an app that treated it as a
 * denial would nag a user who has already made a deliberate, narrower choice.
 */
enum class MediaAccess {
    /** Every photo and video on the device. */
    Full,

    /** Only the items the user picked in the system photo picker. */
    Partial,

    /** Nothing. */
    None,
}

/** Reads the platform's current answer. A port so a test does not need a real package manager. */
interface MediaAccessProbe {
    /** What this app may read right now. */
    fun current(): MediaAccess

    /** The permissions to ask for on this OS version, in the order the platform expects. */
    fun requestedPermissions(): List<String>
}

/**
 * The platform implementation, which has to ask a different question per OS generation.
 *
 * Three eras, and they are not reducible to one call: below Android 13 media lives behind the
 * broad storage permission, Android 13 split it per media type, and Android 14 added the
 * user-selected grant that produces [MediaAccess.Partial]. Asking only the newest question
 * would report [MediaAccess.None] on a device that has full access through the older one.
 */
class AndroidMediaAccessProbe(
    private val context: Context,
) : MediaAccessProbe {
    override fun current(): MediaAccess =
        when {
            granted(Manifest.permission.READ_MEDIA_IMAGES) -> MediaAccess.Full
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE &&
                granted(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED) -> MediaAccess.Partial

            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU &&
                granted(Manifest.permission.READ_EXTERNAL_STORAGE) -> MediaAccess.Full

            else -> MediaAccess.None
        }

    override fun requestedPermissions(): List<String> =
        when {
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE ->
                listOf(
                    Manifest.permission.READ_MEDIA_IMAGES,
                    Manifest.permission.READ_MEDIA_VIDEO,
                    Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED,
                )

            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU ->
                listOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO)

            else -> listOf(Manifest.permission.READ_EXTERNAL_STORAGE)
        }

    private fun granted(permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED
}
