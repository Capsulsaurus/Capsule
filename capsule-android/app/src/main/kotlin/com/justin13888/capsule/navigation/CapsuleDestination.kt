package com.justin13888.capsule.navigation

import androidx.annotation.StringRes
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Collections
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.outlined.Collections
import androidx.compose.material.icons.outlined.PhotoLibrary
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Share
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.GatedView
import kotlinx.serialization.Serializable

/**
 * Every place the app can be.
 *
 * The whole destination surface is declared here, in one sealed hierarchy, even though most
 * of it still renders a placeholder. Two reasons, both about the seam rather than about
 * today's screens:
 *
 * 1. A feature module never sees this file — it depends on `core:*` only, and the app module
 *    is the one place features meet. So the *shell* has to own the routing vocabulary, and
 *    declaring it whole means a feature slice adds a screen without touching navigation.
 * 2. `when` over a sealed hierarchy is exhaustive, so the compiler — not review — catches a
 *    destination that nothing renders.
 *
 * Identifiers are carried as `String`, not as the `core:model` value classes (`AssetId` and
 * friends). A nav key is serialized into saved instance state by reflection
 * (`androidx.navigation3.runtime.serialization.NavKeySerializer`), and an inline value class
 * over `String` is a needless serialization hazard for no type safety that survives the
 * round trip. The value class is re-applied at the feature boundary, where the id is used.
 */
sealed interface CapsuleDestination : NavKey {
    /** Already-in-the-catalog title for this destination's chrome. */
    @get:StringRes
    val titleRes: Int

    /**
     * Whether this destination owns the whole window.
     *
     * Immersive destinations hide the navigation suite at every width: a photo shown beside a
     * navigation rail is a photo the user cannot judge, and an onboarding step with tabs under
     * it invites the user to skip the step.
     */
    val immersive: Boolean get() = false

    /**
     * The view whose fresh-local-auth grant this destination needs, or `null` for the great
     * majority that need none.
     *
     * Modelled on the destination rather than on the entry that renders it so the requirement
     * travels with the route: a deep link, a restored back stack and a pane-scaffold detour all
     * arrive at the same key and therefore hit the same gate. See
     * [com.justin13888.capsule.security.LocalAuthGate] for the grace-window semantics (SR1).
     */
    val gatedView: GatedView? get() = null

    /**
     * One of the five destinations reachable from the navigation suite at any moment.
     *
     * Each owns an independent section of the app; the back stack is rewound to a top-level
     * destination rather than pushed onto when the user switches, which is what makes the bar,
     * the rail and the drawer interchangeable renderings of the same model.
     */
    sealed interface TopLevel : CapsuleDestination {
        /** Shown while this section is the current one. */
        val selectedIcon: ImageVector

        /** Shown while some other section is current. */
        val unselectedIcon: ImageVector
    }

    /** The main library listing: every asset the account can see, newest first. */
    @Serializable
    data object Timeline : TopLevel {
        override val titleRes = R.string.nav_photos
        override val selectedIcon = Icons.Filled.PhotoLibrary
        override val unselectedIcon = Icons.Outlined.PhotoLibrary
    }

    /** Albums, smart albums, people, places and the utility views, grouped. */
    @Serializable
    data object Collections : TopLevel {
        override val titleRes = R.string.nav_collections
        override val selectedIcon = Icons.Filled.Collections
        override val unselectedIcon = Icons.Outlined.Collections
    }

    /** Search across the library, and the saved searches that become smart albums. */
    @Serializable
    data object Search : TopLevel {
        override val titleRes = R.string.nav_search
        override val selectedIcon = Icons.Filled.Search
        override val unselectedIcon = Icons.Outlined.Search
    }

    /** Everything leaving or entering the library through a link: shares, drops, uploads. */
    @Serializable
    data object Sharing : TopLevel {
        override val titleRes = R.string.nav_sharing
        override val selectedIcon = Icons.Filled.Share
        override val unselectedIcon = Icons.Outlined.Share
    }

    /** App and account settings, and the doors to the security and recovery surfaces. */
    @Serializable
    data object Settings : TopLevel {
        override val titleRes = R.string.nav_settings
        override val selectedIcon = Icons.Filled.Settings
        override val unselectedIcon = Icons.Outlined.Settings
    }

    // ---- Library and collections -------------------------------------------------------

    /** One album's contents. */
    @Serializable
    data class AlbumDetail(
        val albumId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_album_detail
    }

    /** One stack — a burst, a bracket, a raw/JPEG pair — expanded into its members. */
    @Serializable
    data class StackDetail(
        val stackId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_stack_detail
    }

    /** The saved searches that present themselves as albums. */
    @Serializable
    data object SmartAlbums : CapsuleDestination {
        override val titleRes = R.string.android_destination_smart_albums
    }

    /** The rule builder behind a smart album; a null id builds a new one. */
    @Serializable
    data class SmartAlbumBuilder(
        val smartAlbumId: String? = null,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_smart_album_builder
    }

    /** Recently deleted. Gated: SR1 treats it as a view a borrowed phone must not open. */
    @Serializable
    data object Trash : CapsuleDestination {
        override val titleRes = R.string.nav_trash
        override val gatedView = GatedView.RecentlyDeleted
    }

    /** Assets the user hid. Gated independently of [Trash] — one grant never opens both. */
    @Serializable
    data object Hidden : CapsuleDestination {
        override val titleRes = R.string.nav_hidden
        override val gatedView = GatedView.Hidden
    }

    /** People and pets found across the library. */
    @Serializable
    data object People : CapsuleDestination {
        override val titleRes = R.string.android_destination_people
    }

    /** Everything attributed to one person. */
    @Serializable
    data class PersonDetail(
        val personId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_person_detail
    }

    /** The library laid out by where it was taken. */
    @Serializable
    data object Places : CapsuleDestination {
        override val titleRes = R.string.android_destination_places
    }

    /** Candidate duplicates awaiting a keep-or-drop decision. */
    @Serializable
    data object Duplicates : CapsuleDestination {
        override val titleRes = R.string.android_destination_duplicates
    }

    // ---- Viewer ------------------------------------------------------------------------

    /**
     * One asset, full bleed.
     *
     * @param contextId the album, person or search the viewer was opened from, so paging
     *   stays inside the listing the user was looking at rather than the whole library.
     */
    @Serializable
    data class AssetViewer(
        val assetId: String,
        val contextId: String? = null,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_asset_viewer
        override val immersive = true
    }

    /** The asset's metadata. A destination of its own so it can also be a supporting pane. */
    @Serializable
    data class AssetInfo(
        val assetId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_asset_info
    }

    /** The signed chain of edits behind one asset. */
    @Serializable
    data class Provenance(
        val assetId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_provenance
    }

    /** Editing the metadata the user owns — title, description, date, place. */
    @Serializable
    data class MetadataEdit(
        val assetId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_metadata_edit
    }

    // ---- Import ------------------------------------------------------------------------

    /** Choosing where an import run reads from. */
    @Serializable
    data object ImportSources : CapsuleDestination {
        override val titleRes = R.string.android_destination_import_sources
    }

    /** What the planner decided, shown before a byte moves. */
    @Serializable
    data class ImportPlan(
        val importId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_import_plan
    }

    /** A running import. */
    @Serializable
    data class ImportProgress(
        val importId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_import_progress
    }

    // ---- Sharing, drops and upload links -----------------------------------------------

    /** Links handed out of the library. */
    @Serializable
    data object ShareLinks : CapsuleDestination {
        override val titleRes = R.string.android_destination_share_links
    }

    /** Minting a share link, optionally pre-scoped to an album. */
    @Serializable
    data class ShareLinkCreate(
        val albumId: String? = null,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_share_link_create
    }

    /** One share link: its scope, its expiry, and who has opened it. */
    @Serializable
    data class ShareLinkDetail(
        val shareId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_share_link_detail
    }

    /** Links that let someone send files in without seeing anything already here. */
    @Serializable
    data object UploadLinks : CapsuleDestination {
        override val titleRes = R.string.android_destination_upload_links
    }

    /** Minting an upload link. */
    @Serializable
    data object UploadLinkCreate : CapsuleDestination {
        override val titleRes = R.string.android_destination_upload_link_create
    }

    /** One upload link and what has arrived through it. */
    @Serializable
    data class UploadLinkDetail(
        val uploadLinkId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_upload_link_detail
    }

    /** What arrived through an upload link and is still awaiting review. */
    @Serializable
    data object DropInbox : CapsuleDestination {
        override val titleRes = R.string.android_destination_drop_inbox
    }

    // ---- Sync, storage and quota -------------------------------------------------------

    /** What is behind, what is ahead, and what the connection class allows. */
    @Serializable
    data object SyncCentre : CapsuleDestination {
        override val titleRes = R.string.android_destination_sync_centre
    }

    /** Uploads still owed to the server. */
    @Serializable
    data object UploadQueue : CapsuleDestination {
        override val titleRes = R.string.android_destination_upload_queue
    }

    /** Items the client refused to accept as-is and parked for a decision. */
    @Serializable
    data object Quarantine : CapsuleDestination {
        override val titleRes = R.string.android_destination_quarantine
    }

    /** What the library occupies, locally and remotely. */
    @Serializable
    data object Storage : CapsuleDestination {
        override val titleRes = R.string.storage_title
    }

    /** The account's allowance and how close it is to the wall. */
    @Serializable
    data object Quota : CapsuleDestination {
        override val titleRes = R.string.android_destination_quota
    }

    // ---- Security, recovery, moderation and audit --------------------------------------

    /** The single door to everything that protects the account. */
    @Serializable
    data object SecurityHub : CapsuleDestination {
        override val titleRes = R.string.security_title
    }

    /** Signed-in sessions and enrolled devices, with revocation. */
    @Serializable
    data object SessionsAndDevices : CapsuleDestination {
        override val titleRes = R.string.security_sessions_title
    }

    /** Passkeys registered against the account. */
    @Serializable
    data object Passkeys : CapsuleDestination {
        override val titleRes = R.string.security_passkeys_title
    }

    /** Whether the account keys can still be recovered, and by what. */
    @Serializable
    data object RecoveryStatus : CapsuleDestination {
        override val titleRes = R.string.android_destination_recovery_status
    }

    /** Proving a recovery secret still opens the account, before it is needed. */
    @Serializable
    data object RecoveryVerify : CapsuleDestination {
        override val titleRes = R.string.android_destination_recovery_verify
    }

    /** Re-wrapping the account master key under a new secret. */
    @Serializable
    data object RecoveryRewrap : CapsuleDestination {
        override val titleRes = R.string.android_destination_recovery_rewrap
    }

    /** The guardians holding shares of a split recovery secret. */
    @Serializable
    data object Shamir : CapsuleDestination {
        override val titleRes = R.string.android_destination_shamir
    }

    /** Writing an encrypted backup of the library out. */
    @Serializable
    data object BackupExport : CapsuleDestination {
        override val titleRes = R.string.android_destination_backup_export
    }

    /** Reading an encrypted backup of the library back. */
    @Serializable
    data object BackupRestore : CapsuleDestination {
        override val titleRes = R.string.android_destination_backup_restore
    }

    /** Reporting an account, a server or a shared item. */
    @Serializable
    data class ModerationReport(
        val subjectId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_moderation_report
    }

    /** Accounts and servers this account refuses to hear from. */
    @Serializable
    data object ModerationBlocks : CapsuleDestination {
        override val titleRes = R.string.android_destination_moderation_blocks
    }

    /** Contesting a moderation decision taken against this account. */
    @Serializable
    data class ModerationAppeal(
        val caseId: String,
    ) : CapsuleDestination {
        override val titleRes = R.string.android_destination_moderation_appeal
    }

    /** Security-relevant events on the account, newest first. */
    @Serializable
    data object AuditLog : CapsuleDestination {
        override val titleRes = R.string.android_destination_audit_log
    }

    // ---- Settings sub-pages ------------------------------------------------------------

    /** Theme, contrast and density. */
    @Serializable
    data object SettingsAppearance : CapsuleDestination {
        override val titleRes = R.string.android_destination_settings_appearance
    }

    /** How the library itself behaves: grouping, gated-view grace, default album. */
    @Serializable
    data object SettingsLibrary : CapsuleDestination {
        override val titleRes = R.string.android_destination_settings_library
    }

    /** What may run on which connection class, and when. */
    @Serializable
    data object SettingsSync : CapsuleDestination {
        override val titleRes = R.string.android_destination_settings_sync
    }

    /** Consent, diagnostics and what leaves the device. */
    @Serializable
    data object SettingsPrivacy : CapsuleDestination {
        override val titleRes = R.string.android_destination_settings_privacy
    }

    /** Versions, licences and the protocol range this build speaks. */
    @Serializable
    data object SettingsAbout : CapsuleDestination {
        override val titleRes = R.string.android_destination_settings_about
    }

    // ---- Onboarding and enrollment -----------------------------------------------------

    /** First run, before a server is known. */
    @Serializable
    data object OnboardingWelcome : CapsuleDestination {
        override val titleRes = R.string.android_destination_onboarding_welcome
        override val immersive = true
    }

    /** Naming the server that holds the library. */
    @Serializable
    data object OnboardingServer : CapsuleDestination {
        override val titleRes = R.string.android_destination_onboarding_server
        override val immersive = true
    }

    /** Authenticating against the chosen server. */
    @Serializable
    data object OnboardingSignIn : CapsuleDestination {
        override val titleRes = R.string.android_destination_onboarding_sign_in
        override val immersive = true
    }

    /** Creating the passphrase that wraps the account keys. */
    @Serializable
    data object OnboardingPassphrase : CapsuleDestination {
        override val titleRes = R.string.android_destination_onboarding_passphrase
        override val immersive = true
    }

    /** The hand-off from onboarding into the library. */
    @Serializable
    data object OnboardingComplete : CapsuleDestination {
        override val titleRes = R.string.android_destination_onboarding_complete
        override val immersive = true
    }

    /** Starting the enrollment of this device against an already-trusted one. */
    @Serializable
    data object EnrollmentStart : CapsuleDestination {
        override val titleRes = R.string.android_destination_enrollment_start
        override val immersive = true
    }

    /** This device reading the code shown by an enrolled one. */
    @Serializable
    data object EnrollmentScan : CapsuleDestination {
        override val titleRes = R.string.android_destination_enrollment_scan
        override val immersive = true
    }

    /** This device showing the code an enrolled one must approve. */
    @Serializable
    data object EnrollmentCode : CapsuleDestination {
        override val titleRes = R.string.android_destination_enrollment_code
        override val immersive = true
    }

    /** An enrolled device approving a new one. */
    @Serializable
    data object EnrollmentApprove : CapsuleDestination {
        override val titleRes = R.string.android_destination_enrollment_approve
        override val immersive = true
    }

    /** Enrollment done; keys are wrapped for this device. */
    @Serializable
    data object EnrollmentComplete : CapsuleDestination {
        override val titleRes = R.string.android_destination_enrollment_complete
        override val immersive = true
    }

    companion object {
        /** The navigation suite's items, in the order they are shown. */
        val topLevel: List<TopLevel> = listOf(Timeline, Collections, Search, Sharing, Settings)

        /** Where the app opens, and the floor the back stack is never popped past. */
        val start: TopLevel = Timeline
    }
}
