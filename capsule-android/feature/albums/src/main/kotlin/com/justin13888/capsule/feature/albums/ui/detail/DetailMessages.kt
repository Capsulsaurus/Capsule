package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.model.MembershipRefusal

/**
 * What album detail says about the last thing the user did.
 *
 * Every branch names a *reason*, never a status: "an album needs at least one admin" is
 * something a person can act on, while "403" is something they can only be annoyed by.
 */
@Composable
internal fun detailMessageText(message: AlbumDetailMessage): String =
    when (message) {
        is AlbumDetailMessage.Refused -> refusalText(message.reason)
        is AlbumDetailMessage.Failed -> failureText(message.code)
        AlbumDetailMessage.DefaultUndeletable -> stringResource(R.string.albums_delete_default_refused)
    }

@Composable
private fun refusalText(reason: MembershipRefusal): String =
    stringResource(
        when (reason) {
            MembershipRefusal.NotAdmin -> R.string.albums_members_admin_only
            MembershipRefusal.NotAContainer -> R.string.error_album_not_available
            MembershipRefusal.AlreadyMember -> R.string.albums_members_already_member
            MembershipRefusal.EmptyRecipient -> R.string.albums_members_empty_recipient
            MembershipRefusal.LastAdmin -> R.string.albums_members_last_admin
        },
    )

/**
 * A repository failure, localized from its stable catalogue code.
 *
 * A code with no wording of its own falls back to the generic message rather than showing the
 * English detail, which exists for a support bundle and not for a person.
 */
@Composable
private fun failureText(code: String?): String =
    stringResource(
        when (code) {
            "error.album.invalid_id" -> R.string.error_album_invalid_id
            "error.album.not_available" -> R.string.error_album_not_available
            "error.upload.album_access_denied" -> R.string.error_upload_album_access_denied
            "error.quota.grace_locked" -> R.string.error_quota_grace_locked
            else -> R.string.albums_error_write
        },
    )
