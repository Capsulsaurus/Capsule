package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.DetectionMethod
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.model.HistoryPolicy
import com.justin13888.capsule.feature.albums.model.RetentionCountdown
import com.justin13888.capsule.feature.albums.model.SystemViewKind

/**
 * The name of a member's capability.
 *
 * Takes the [Wire] rather than the constant because a capability written by a newer peer has
 * to render as *something*: dropping the row would hide a member the album really has, and
 * guessing at the tier would misstate what they can do.
 */
@Composable
internal fun capabilityLabel(capability: Wire<AlbumCapability>?): String =
    stringResource(
        when (capability?.knownOrNull) {
            AlbumCapability.Read -> R.string.albums_role_read
            AlbumCapability.Write -> R.string.albums_role_write
            AlbumCapability.Admin -> R.string.albums_role_admin
            null -> R.string.albums_role_unknown
        },
    )

/** What a `Welcome` delivers to a new member of this album. */
@Composable
internal fun historyPolicyLabel(policy: HistoryPolicy): String =
    when (policy) {
        HistoryPolicy.Full -> stringResource(R.string.albums_policy_history_full)
        is HistoryPolicy.Capped ->
            stringResource(R.string.albums_policy_history_capped).withArgs("days" to policy.window.toDays())
    }

/** The name of a stack's kind, including the honest answer for a token from a newer peer. */
@Composable
@Suppress("CyclomaticComplexMethod")
internal fun stackTypeLabel(type: Wire<StackType>): String =
    stringResource(
        when (type.knownOrNull) {
            StackType.RawJpeg -> R.string.albums_stack_type_raw_jpeg
            StackType.Burst -> R.string.albums_stack_type_burst
            StackType.LivePhoto -> R.string.albums_stack_type_live_photo
            StackType.Portrait -> R.string.albums_stack_type_portrait
            StackType.SmartSelection -> R.string.albums_stack_type_smart_selection
            StackType.HdrBracket -> R.string.albums_stack_type_hdr_bracket
            StackType.FocusStack -> R.string.albums_stack_type_focus_stack
            StackType.PixelShift -> R.string.albums_stack_type_pixel_shift
            StackType.Panorama -> R.string.albums_stack_type_panorama
            StackType.Proxy -> R.string.albums_stack_type_proxy
            StackType.Chaptered -> R.string.albums_stack_type_chaptered
            StackType.DualAudio -> R.string.albums_stack_type_dual_audio
            StackType.Custom -> R.string.albums_stack_type_custom
            null -> R.string.albums_stack_type_unknown
        },
    )

/** Which detector produced a grouping the user did not make, or `null` when they did. */
@Composable
internal fun detectionLabel(method: Wire<DetectionMethod>?): String? {
    val known = method?.knownOrNull ?: return null
    val how =
        stringResource(
            when (known) {
                DetectionMethod.FilenameStem -> R.string.albums_stack_detection_filename_stem
                DetectionMethod.ContentIdentifier -> R.string.albums_stack_detection_content_identifier
                DetectionMethod.Timecode -> R.string.albums_stack_detection_timecode
                DetectionMethod.Manual -> R.string.albums_stack_detection_manual
            },
        )
    return stringResource(R.string.albums_stack_auto_detected).withArgs("method" to how)
}

/** How much of a trashed asset's signed retention window is left, in words. */
@Composable
internal fun retentionLabel(countdown: RetentionCountdown): String =
    when (countdown) {
        is RetentionCountdown.Days ->
            stringResource(R.string.albums_trash_retention_days).withArgs("days" to countdown.days)

        is RetentionCountdown.Hours ->
            stringResource(R.string.albums_trash_retention_hours).withArgs("hours" to countdown.hours)

        RetentionCountdown.Expired -> stringResource(R.string.albums_trash_retention_expired)
    }

/** The name of one built-in view. */
@Composable
internal fun systemViewLabel(kind: SystemViewKind): String =
    stringResource(
        when (kind) {
            SystemViewKind.Videos -> R.string.albums_media_type_videos
            SystemViewKind.LivePhotos -> R.string.albums_media_type_live_photos
            SystemViewKind.Bursts -> R.string.albums_media_type_bursts
            SystemViewKind.Panoramas -> R.string.albums_media_type_panoramas
            SystemViewKind.Raw -> R.string.albums_media_type_raw
        },
    )

/**
 * The localized message for a repository failure.
 *
 * Keyed on the stable `error.*` code, never on an HTTP status: the status says nothing the
 * user can act on and two deployments may spell the same condition differently. A code this
 * screen has no specific wording for falls back to the generic message rather than showing
 * the English detail, which is for a support bundle and not for a person.
 */
@Composable
internal fun errorText(error: CapsuleError): String =
    stringResource(
        when (error.code?.value) {
            "error.album.invalid_id" -> R.string.error_album_invalid_id
            "error.album.not_available" -> R.string.error_album_not_available
            "error.upload.album_access_denied" -> R.string.error_upload_album_access_denied
            "error.protocol.version_unsupported" -> R.string.error_protocol_version_unsupported
            "error.quota.exceeded" -> R.string.error_quota_exceeded
            "error.quota.grace_locked" -> R.string.error_quota_grace_locked
            "error.federation.capability_expired" -> R.string.error_federation_capability_expired
            "error.federation.capability_revoked" -> R.string.error_federation_capability_revoked
            "error.federation.circuit_open" -> R.string.error_federation_circuit_open
            else -> R.string.common_error_generic
        },
    )
