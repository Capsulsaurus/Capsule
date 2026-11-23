package com.justin13888.capsule.feature.settings

import com.justin13888.capsule.core.i18n.R

/**
 * The pages settings is divided into.
 *
 * Each one is also a real destination in the app's routing table, which is why the key is a
 * stable string rather than the enum's ordinal: the list-detail scaffold carries the chosen
 * pane as a content key, and an ordinal would silently re-point every open pane the day a
 * section is inserted in the middle.
 *
 * Language sits inside Appearance and the server details sit inside About. Neither has a
 * destination of its own, and giving them one would put two rows in the root list that nobody
 * looks for there — a user hunting for the app's language looks under how the app looks.
 */
enum class SettingsSection(
    /** The stable key this section travels as. */
    val key: String,
    /** The already-catalogued page title. */
    val titleRes: Int,
    /** The root list's supporting line. */
    val summaryRes: Int,
) {
    /** Theme, colour, contrast, grid density, motion, and the app's language. */
    Appearance(
        key = "appearance",
        titleRes = R.string.android_destination_settings_appearance,
        summaryRes = R.string.android_settings_appearance_summary,
    ),

    /** Import sources, trash retention, and the gated-view grace window. */
    Library(
        key = "library",
        titleRes = R.string.android_destination_settings_library,
        summaryRes = R.string.android_settings_library_summary,
    ),

    /** Auto-sync, download scope, upload order, connection rules and warnings. */
    Sync(
        key = "sync",
        titleRes = R.string.android_destination_settings_sync,
        summaryRes = R.string.android_settings_sync_summary,
    ),

    /** What leaves the device on an export, and whether diagnostics are sent. */
    Privacy(
        key = "privacy",
        titleRes = R.string.android_destination_settings_privacy,
        summaryRes = R.string.android_settings_privacy_summary,
    ),

    /** Version, the server and protocol this build talks to, and licences. */
    About(
        key = "about",
        titleRes = R.string.android_destination_settings_about,
        summaryRes = R.string.android_settings_about_summary,
    ),
    ;

    companion object {
        /** The section [key] names, or `null` when it names none. */
        fun byKey(key: String?): SettingsSection? = entries.firstOrNull { it.key == key }
    }
}
