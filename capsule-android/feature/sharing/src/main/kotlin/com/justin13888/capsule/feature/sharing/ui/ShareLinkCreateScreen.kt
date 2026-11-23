package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.sharing.AlbumOption
import com.justin13888.capsule.feature.sharing.ShareExpiry
import com.justin13888.capsule.feature.sharing.ShareLinkCreateUiState
import com.justin13888.capsule.feature.sharing.ShareScopeChoice
import com.justin13888.capsule.feature.sharing.StrippedField
import com.justin13888.capsule.feature.sharing.failureMessage
import com.justin13888.capsule.feature.sharing.failureTitle

/** Test tag on the submit button. */
const val SHARE_CREATE_SUBMIT_TAG: String = "share-create-submit"

/** Test tag on the mandatory privacy-strip disclosure. */
const val SHARE_CREATE_DISCLOSURE_TAG: String = "share-create-disclosure"

private val FormPadding = 16.dp
private val FormSpacing = 8.dp

/** Everything the create form can be asked to do. */
@Immutable
class ShareLinkCreateActions(
    /** Switches between sharing one item and a whole album. */
    val selectScope: (ShareScopeChoice) -> Unit,
    /** Picks which album an album-scoped link points at. */
    val selectAlbum: (AlbumId) -> Unit,
    /** Records the typed passphrase. */
    val setPassphrase: (String) -> Unit,
    /** Picks how long the link stays live. */
    val selectExpiry: (ShareExpiry) -> Unit,
    /** Mints the link. */
    val submit: () -> Unit,
    /** Re-reads the album list after a failure. */
    val retry: () -> Unit,
    /** Leaves without creating anything. */
    val back: () -> Unit,
)

/**
 * Minting a share link.
 *
 * The privacy-strip disclosure is a permanent part of this form and is deliberately not a
 * control. The serve path always removes the camera serial, the device and session ids, the
 * precise fix and the contact tags, with no per-share opt-out — so a switch here could only
 * be a switch that does nothing, or a claim that the fields are sometimes kept. Stating the
 * fact is the honest version of the same square centimetre.
 */
@Composable
fun ShareLinkCreateScreen(
    state: ShareLinkCreateUiState,
    actions: ShareLinkCreateActions,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_share_link_create),
                navigationIcon = { BackButton(actions.back) },
            )
        },
    ) { insets ->
        Column(modifier = Modifier.padding(insets).fillMaxSize()) {
            when {
                state.isLoading ->
                    CapsuleLoadingState(
                        title = stringResource(R.string.android_sharing_loading),
                        contentDescription = stringResource(R.string.android_sharing_loading),
                    )

                state.failure != null && state.albums.isEmpty() ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(
                                onClick = actions.retry,
                                label = stringResource(R.string.android_sharing_retry),
                            )
                        },
                    )

                else -> CreateForm(state, actions)
            }
        }
    }
}

@Composable
private fun CreateForm(
    state: ShareLinkCreateUiState,
    actions: ShareLinkCreateActions,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = FormPadding),
    ) {
        item { PrivacyDisclosure(state.strippedFields) }
        item { CapsuleSectionHeader(title = stringResource(R.string.android_sharing_scope_label)) }
        item {
            CapsuleRadioRow(
                label = stringResource(R.string.android_sharing_scope_asset),
                supporting =
                    if (state.canScopeToAsset) {
                        null
                    } else {
                        stringResource(R.string.android_sharing_scope_asset_unavailable)
                    },
                selected = state.scope == ShareScopeChoice.SingleAsset,
                enabled = state.canScopeToAsset,
                onSelect = { actions.selectScope(ShareScopeChoice.SingleAsset) },
            )
        }
        item {
            CapsuleRadioRow(
                label = stringResource(R.string.android_sharing_scope_album),
                selected = state.scope == ShareScopeChoice.WholeAlbum,
                onSelect = { actions.selectScope(ShareScopeChoice.WholeAlbum) },
            )
        }
        if (state.scope == ShareScopeChoice.WholeAlbum) {
            items(items = state.albums, key = { it.id.value }) { album ->
                CapsuleRadioRow(
                    label = albumLabel(album),
                    selected = state.selectedAlbumId == album.id,
                    onSelect = { actions.selectAlbum(album.id) },
                    modifier = Modifier.padding(start = FormPadding),
                )
            }
        }
        item { CapsuleSectionHeader(title = stringResource(R.string.android_sharing_expiry_label)) }
        items(items = ShareExpiry.entries.toList(), key = { it.name }) { option ->
            CapsuleRadioRow(
                label = expiryLabel(option),
                selected = state.expiry == option,
                onSelect = { actions.selectExpiry(option) },
            )
        }
        item { PassphraseField(state, actions) }
        if (state.failure != null) {
            item {
                CapsuleBanner(
                    title = failureTitle(state.failure),
                    body = failureMessage(state.failure),
                    severity = CapsuleBannerSeverity.Error,
                    modifier = Modifier.fillMaxWidth().padding(FormPadding),
                )
            }
        }
        item {
            CapsuleButton(
                onClick = actions.submit,
                label =
                    if (state.isSubmitting) {
                        stringResource(R.string.android_sharing_create_busy)
                    } else {
                        stringResource(R.string.sharing_create_link)
                    },
                enabled = state.canSubmit,
                modifier = Modifier.fillMaxWidth().padding(FormPadding).testTag(SHARE_CREATE_SUBMIT_TAG),
            )
        }
    }
}

@Composable
private fun PassphraseField(
    state: ShareLinkCreateUiState,
    actions: ShareLinkCreateActions,
) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = FormPadding)) {
        OutlinedTextField(
            value = state.passphrase,
            onValueChange = actions.setPassphrase,
            label = { Text(text = stringResource(R.string.android_sharing_passphrase_label)) },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            text = stringResource(R.string.android_sharing_passphrase_help),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = FormSpacing / 2),
        )
    }
}

/**
 * The strip, stated as a fact.
 *
 * Rendered as a banner rather than a footnote because it is the one thing on this form the
 * user cannot change and the one thing they most need to have read.
 */
@Composable
private fun PrivacyDisclosure(fields: List<StrippedField>) {
    // `map` is inline, so the composable lookups happen in this composition; `joinToString`
    // is not, which is why the labels are resolved first rather than inside the join.
    val labels = fields.map { field -> BULLET + strippedFieldLabel(field) }
    CapsuleBanner(
        title = stringResource(R.string.android_sharing_privacy_title),
        body = stringResource(R.string.android_sharing_privacy_body) + labels.joinToString("\n", prefix = "\n"),
        severity = CapsuleBannerSeverity.Info,
        icon = Icons.Filled.Shield,
        modifier = Modifier.fillMaxWidth().padding(FormPadding).testTag(SHARE_CREATE_DISCLOSURE_TAG),
    )
}

/** The bullet each disclosed field is listed behind. */
private const val BULLET: String = "\u2022 "

/** What one always-removed field is called. */
@Composable
internal fun strippedFieldLabel(field: StrippedField): String =
    when (field) {
        StrippedField.CameraSerial -> stringResource(R.string.android_sharing_privacy_camera_serial)
        StrippedField.DeviceAndSessionIds -> stringResource(R.string.android_sharing_privacy_device_ids)
        StrippedField.PreciseLocation -> stringResource(R.string.android_sharing_privacy_location)
        StrippedField.ContactTags -> stringResource(R.string.android_sharing_privacy_contact_tags)
    }

/** How long a preset keeps the link alive. */
@Composable
internal fun expiryLabel(expiry: ShareExpiry): String =
    when (expiry) {
        ShareExpiry.Never -> stringResource(R.string.android_sharing_expiry_never)
        ShareExpiry.OneDay -> stringResource(R.string.android_sharing_expiry_one_day)
        ShareExpiry.OneWeek -> stringResource(R.string.android_sharing_expiry_one_week)
        ShareExpiry.OneMonth -> stringResource(R.string.android_sharing_expiry_one_month)
    }

/** An album's own name, or the standing name for the nameless default container. */
@Composable
internal fun albumLabel(album: AlbumOption): String =
    album.name.takeIf(String::isNotBlank) ?: stringResource(R.string.android_sharing_album_default)
