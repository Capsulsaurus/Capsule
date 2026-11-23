package com.justin13888.capsule.feature.settings

import android.app.LocaleManager
import android.content.Context
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import android.os.LocaleList
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Immutable

/**
 * Applies the chosen app language to the running process.
 *
 * A port rather than a call, because Android only grew a per-app language setting in API 33.
 * Below that there is genuinely nothing to call, and [isSupported] says so out loud so the
 * screen can tell the user where to go instead — rather than accepting a choice that silently
 * does nothing.
 */
interface LocaleApplier {
    /** Whether this platform has a per-app language setting at all. */
    val isSupported: Boolean

    /** Applies [languageTag], or hands the decision back to the device when it is `null`. */
    fun apply(languageTag: String?)
}

/** The real applier: `LocaleManager` on API 33+, and a no-op with [isSupported] false below. */
class PlatformLocaleApplier(
    private val context: Context,
) : LocaleApplier {
    override val isSupported: Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU

    override fun apply(languageTag: String?) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) applyLocales(languageTag)
    }

    @RequiresApi(Build.VERSION_CODES.TIRAMISU)
    private fun applyLocales(languageTag: String?) {
        val manager = context.getSystemService(LocaleManager::class.java) ?: return
        manager.applicationLocales =
            if (languageTag == null) LocaleList.getEmptyLocaleList() else LocaleList.forLanguageTags(languageTag)
    }
}

/**
 * What this build is, for the About page.
 *
 * A value read once at graph assembly rather than a `Context` handed to a view model: the
 * version cannot change while the process lives, and a view model that could reach a
 * `Context` is a view model that eventually does something else with it.
 */
@Immutable
data class AppBuildInfo(
    /** The user-facing version name. */
    val versionName: String,
    /** The monotonic build number behind it. */
    val versionCode: Long,
) {
    companion object {
        /** What the About page shows when the package manager will not answer. */
        val UNKNOWN: AppBuildInfo = AppBuildInfo(versionName = "—", versionCode = 0)
    }
}

/**
 * The protocol range this build speaks.
 *
 * A build fact rather than a preference, and stated on the About page because it is the one
 * number that explains a `426` refusal: a server that has moved past this range will not
 * accept writes from this app until it is updated.
 */
object CapsuleProtocol {
    /** The oldest protocol version this build can read and write. */
    const val MINIMUM: String = "1.0"

    /** The newest protocol version this build can read and write. */
    const val MAXIMUM: String = "1.0"

    /** The range as the About page prints it. */
    val RANGE: String = if (MINIMUM == MAXIMUM) MINIMUM else "$MINIMUM – $MAXIMUM"
}

/**
 * This build's version, or [AppBuildInfo.UNKNOWN] when the package manager will not say.
 *
 * Read through the package manager rather than through `BuildConfig`, because no library
 * module in this project generates a `BuildConfig` and turning it on for one string would be
 * a build-wide change for a single field.
 */
fun Context.capsuleBuildInfo(): AppBuildInfo =
    try {
        val info = packageManager.packageInfoOf(packageName)
        AppBuildInfo(
            versionName = info.versionName ?: AppBuildInfo.UNKNOWN.versionName,
            versionCode = info.longVersionCodeCompat(),
        )
    } catch (_: PackageManager.NameNotFoundException) {
        // Reachable while the package is being replaced. An About page that crashed there
        // would be a crash in the one screen a user opens to report a problem.
        AppBuildInfo.UNKNOWN
    }

/** The typed-flags overload on API 33+, and the integer one below it. */
private fun PackageManager.packageInfoOf(packageName: String): PackageInfo =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
    } else {
        @Suppress("DEPRECATION")
        getPackageInfo(packageName, 0)
    }

/** The 64-bit version code on API 28+, and the 32-bit one widened below it. */
private fun PackageInfo.longVersionCodeCompat(): Long =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
        longVersionCode
    } else {
        @Suppress("DEPRECATION")
        versionCode.toLong()
    }

/**
 * One third-party component and the licence it is offered under.
 *
 * Neither field is a catalogue key: a component's name and its licence's SPDX identifier are
 * legal identifiers, and translating either would misstate the notice this list exists to give.
 */
@Immutable
data class CapsuleLicence(
    /** The component, as its authors name it. */
    val component: String,
    /** The SPDX identifier of the licence it is offered under. */
    val licence: String,
)

/** The components Capsule's Android client is built from. */
val CAPSULE_LICENCES: List<CapsuleLicence> =
    listOf(
        CapsuleLicence("Android Jetpack", "Apache-2.0"),
        CapsuleLicence("Jetpack Compose", "Apache-2.0"),
        CapsuleLicence("Material Components for Android", "Apache-2.0"),
        CapsuleLicence("Kotlin standard library", "Apache-2.0"),
        CapsuleLicence("kotlinx.coroutines", "Apache-2.0"),
        CapsuleLicence("kotlinx.serialization", "Apache-2.0"),
        CapsuleLicence("kotlinx.collections.immutable", "Apache-2.0"),
        CapsuleLicence("Koin", "Apache-2.0"),
        CapsuleLicence("Coil", "Apache-2.0"),
        CapsuleLicence("Robolectric", "MIT"),
        CapsuleLicence("Turbine", "Apache-2.0"),
        CapsuleLicence("MockK", "Apache-2.0"),
        CapsuleLicence("Roborazzi", "Apache-2.0"),
    )
