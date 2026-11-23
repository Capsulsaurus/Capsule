package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.i18n.R

/**
 * A failure reduced to the two things the viewer can act on.
 *
 * The stable `error.*` [code] is the contract a client localizes; [detail] is the English
 * message that travels beside it and stays English on purpose so it remains greppable in a
 * support bundle. An HTTP status appears nowhere, because a status tells the user nothing
 * they can do.
 *
 * Modelled as a small value rather than the thrown [CapsuleError] so the UI state stays
 * immutable and comparable — a `StateFlow` holding an `Exception` re-emits on every retry
 * because two exceptions with the same message are never equal.
 */
@Immutable
data class ViewerFailure(
    /** The catalogue key, or `null` for a purely local failure that has none. */
    val code: String?,
    /** The English detail. Deliberately not localized. */
    val detail: String,
    /**
     * Whether there was no usable network path at all.
     *
     * Carried as a flag rather than inferred later from [detail]: offline has no catalogue
     * code — it never crossed a wire to be issued one — and matching on an English sentence
     * to recover the distinction would break the first time that sentence was reworded.
     */
    val isOffline: Boolean = false,
) {
    companion object {
        /** Reduces a thrown repository failure to what the viewer will draw. */
        fun of(error: CapsuleError): ViewerFailure =
            ViewerFailure(
                code = error.code?.value,
                detail = error.detail,
                isOffline = error is LocalError.Offline,
            )
    }
}

/**
 * The localized sentence for a failure.
 *
 * A closed `when` rather than a resource-name lookup by string. Resolving `error.blob.
 * pending_upload` to `R.string.error_blob_pending_upload` at runtime would compile whether or
 * not the key existed, so the first build that dropped one would render a blank line at the
 * user; the branch below stops compiling instead. The set is only the codes a viewer can
 * actually reach, and anything else falls back to the generic sentence with the English
 * detail beneath it — which is still an explanation, never a status code.
 */
@Composable
internal fun ViewerFailure.localizedMessage(): String =
    when {
        isOffline -> stringResource(R.string.android_shell_offline_body)
        else -> localizedCodeMessage()
    }

/** The sentence for this failure's catalogue code, or the generic one when it has none. */
@Composable
private fun ViewerFailure.localizedCodeMessage(): String =
    when (code) {
        "error.blob.pending_upload" -> stringResource(R.string.error_blob_pending_upload)
        "error.album.not_available" -> stringResource(R.string.error_album_not_available)
        "error.protocol.version_unsupported" -> stringResource(R.string.error_protocol_version_unsupported)
        "error.upload.stale_revival" -> stringResource(R.string.error_upload_stale_revival)
        "error.quota.exceeded" -> stringResource(R.string.error_quota_exceeded)
        "error.quota.grace_locked" -> stringResource(R.string.error_quota_grace_locked)
        "error.moderation.account_suspended" -> stringResource(R.string.error_moderation_account_suspended)
        "error.sync.unauthenticated" -> stringResource(R.string.error_sync_unauthenticated)
        "error.sync.cursor_invalid" -> stringResource(R.string.error_sync_cursor_invalid)
        else -> stringResource(R.string.common_error_generic)
    }
