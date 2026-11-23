package com.justin13888.capsule.feature.drops.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
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
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.drops.CapEntry
import com.justin13888.capsule.feature.drops.CapKind
import com.justin13888.capsule.feature.drops.LinkExpiry
import com.justin13888.capsule.feature.drops.UploadLinkCreateUiState
import com.justin13888.capsule.feature.drops.failureMessage
import com.justin13888.capsule.feature.drops.failureTitle

/** Test tag on the submit button. */
const val UPLOAD_CREATE_SUBMIT_TAG: String = "upload-create-submit"

/** Test tag on the cumulative-size cap field. */
const val UPLOAD_CREATE_TOTAL_TAG: String = "upload-create-total"

/** Test tag on the file-count cap field. */
const val UPLOAD_CREATE_COUNT_TAG: String = "upload-create-count"

/** Test tag on the per-file size cap field. */
const val UPLOAD_CREATE_FILE_SIZE_TAG: String = "upload-create-file-size"

/** Test tag on the single-use switch. */
const val UPLOAD_CREATE_SINGLE_USE_TAG: String = "upload-create-single-use"

private val FormPadding = 16.dp
private val FormSpacing = 8.dp

/** Everything the create-upload-link form can be asked to do. */
@Immutable
class UploadLinkCreateActions(
    /** Chooses how long the link accepts deposits. */
    val selectExpiry: (LinkExpiry) -> Unit,
    /** Records one numeric ceiling exactly as typed. */
    val setCap: (CapKind, String) -> Unit,
    /** Sets whether the link closes after one deposit session. */
    val setSingleUse: (Boolean) -> Unit,
    /** Records the typed passphrase. */
    val setPassphrase: (String) -> Unit,
    /** Provisions the link. */
    val submit: () -> Unit,
    /** Leaves without provisioning anything. */
    val back: () -> Unit,
)

/**
 * Provisioning a link that lets a guest send files in.
 *
 * The caps are the substance of this screen, not its fine print: they are the whole of what
 * bounds a leaked link. A stranger holding the URL can spend quota and fill the inbox up to
 * exactly these numbers and no further, and can never reach anything already in the library —
 * adoption happens on a trusted client, which is a different screen entirely.
 */
@Composable
fun UploadLinkCreateScreen(
    state: UploadLinkCreateUiState,
    actions: UploadLinkCreateActions,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_upload_link_create),
                navigationIcon = { BackButton(actions.back) },
            )
        },
    ) { insets ->
        LazyColumn(
            modifier = Modifier.padding(insets).fillMaxSize(),
            contentPadding = PaddingValues(bottom = FormPadding),
        ) {
            item { CapsExplanation() }
            item { CapsuleSectionHeader(title = stringResource(R.string.android_drops_create_expiry_label)) }
            items(LinkExpiry.entries.size) { index ->
                val option = LinkExpiry.entries[index]
                DropsRadioRow(
                    label = expiryLabel(option),
                    selected = state.expiry == option,
                    onSelect = { actions.selectExpiry(option) },
                )
            }
            item { CapsuleSectionHeader(title = stringResource(R.string.android_drops_create_caps_label)) }
            item { CapFields(state, actions) }
            item {
                DropsSwitchRow(
                    label = stringResource(R.string.android_drops_create_single_use_label),
                    supporting = stringResource(R.string.android_drops_create_single_use_help),
                    checked = state.singleUse,
                    onCheckedChange = actions.setSingleUse,
                    modifier = Modifier.testTag(UPLOAD_CREATE_SINGLE_USE_TAG),
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
            item { SubmitButton(state, actions) }
        }
    }
}

@Composable
private fun CapsExplanation() {
    CapsuleBanner(
        title = stringResource(R.string.android_drops_create_caps_title),
        body = stringResource(R.string.android_drops_create_caps_body),
        severity = CapsuleBannerSeverity.Info,
        icon = Icons.Filled.Shield,
        modifier = Modifier.fillMaxWidth().padding(FormPadding),
    )
}

@Composable
private fun CapFields(
    state: UploadLinkCreateUiState,
    actions: UploadLinkCreateActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = FormPadding),
    ) {
        CapField(
            entry = state.entry(CapKind.TotalBytes),
            label = stringResource(R.string.android_drops_create_max_total_label),
            onValueChange = { raw -> actions.setCap(CapKind.TotalBytes, raw) },
            tag = UPLOAD_CREATE_TOTAL_TAG,
        )
        CapField(
            entry = state.entry(CapKind.FileCount),
            label = stringResource(R.string.android_drops_create_max_files_label),
            onValueChange = { raw -> actions.setCap(CapKind.FileCount, raw) },
            tag = UPLOAD_CREATE_COUNT_TAG,
        )
        CapField(
            entry = state.entry(CapKind.FileSize),
            label = stringResource(R.string.android_drops_create_max_file_size_label),
            onValueChange = { raw -> actions.setCap(CapKind.FileSize, raw) },
            tag = UPLOAD_CREATE_FILE_SIZE_TAG,
        )
    }
}

/**
 * One optional numeric cap.
 *
 * An empty field means unbounded and says so under the label, because a blank box next to the
 * word "limit" otherwise reads as a limit the user forgot to set rather than one they chose
 * not to impose.
 */
@Composable
private fun CapField(
    entry: CapEntry,
    label: String,
    onValueChange: (String) -> Unit,
    tag: String,
) {
    Column(modifier = Modifier.fillMaxWidth().padding(vertical = FormSpacing / 2)) {
        OutlinedTextField(
            value = entry.raw,
            onValueChange = onValueChange,
            label = { Text(text = label) },
            singleLine = true,
            isError = entry.isInvalid,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Next),
            modifier = Modifier.fillMaxWidth().testTag(tag),
        )
        Text(
            text =
                if (entry.isInvalid) {
                    stringResource(R.string.android_drops_create_invalid_number)
                } else {
                    stringResource(R.string.android_drops_create_unbounded_hint)
                },
            style = MaterialTheme.typography.bodySmall,
            color =
                if (entry.isInvalid) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
        )
    }
}

@Composable
private fun PassphraseField(
    state: UploadLinkCreateUiState,
    actions: UploadLinkCreateActions,
) {
    Column(modifier = Modifier.fillMaxWidth().padding(FormPadding)) {
        OutlinedTextField(
            value = state.passphrase,
            onValueChange = actions.setPassphrase,
            label = { Text(text = stringResource(R.string.android_drops_create_passphrase_label)) },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            text = stringResource(R.string.android_drops_create_passphrase_help),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = FormSpacing / 2),
        )
    }
}

@Composable
private fun SubmitButton(
    state: UploadLinkCreateUiState,
    actions: UploadLinkCreateActions,
) {
    CapsuleButton(
        onClick = actions.submit,
        label =
            if (state.isSubmitting) {
                stringResource(R.string.android_drops_create_busy)
            } else {
                stringResource(R.string.android_destination_upload_link_create)
            },
        enabled = state.canSubmit,
        modifier = Modifier.fillMaxWidth().padding(horizontal = FormPadding).testTag(UPLOAD_CREATE_SUBMIT_TAG),
    )
}

/** How long a preset keeps the link accepting deposits. */
@Composable
internal fun expiryLabel(expiry: LinkExpiry): String =
    when (expiry) {
        LinkExpiry.Never -> stringResource(R.string.android_drops_expiry_never)
        LinkExpiry.OneDay -> stringResource(R.string.android_drops_expiry_one_day)
        LinkExpiry.OneWeek -> stringResource(R.string.android_drops_expiry_one_week)
        LinkExpiry.OneMonth -> stringResource(R.string.android_drops_expiry_one_month)
    }
