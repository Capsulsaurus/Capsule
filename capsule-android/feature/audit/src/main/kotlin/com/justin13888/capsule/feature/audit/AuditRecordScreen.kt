package com.justin13888.capsule.feature.audit

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId

/** Test handle for the record pane. */
const val AUDIT_RECORD_TAG: String = "audit.record"

/** Test handle for the record pane before anything is selected. */
const val AUDIT_RECORD_EMPTY_TAG: String = "audit.record.empty"

private val PaneInset = 16.dp
private val FieldSpacing = 4.dp

/**
 * The provenance record behind one entry.
 *
 * Shows the hashes rather than summarising them away. They are the reason the log is
 * trustworthy at all: each record names the chain head it followed, so a replayed or missing
 * write is detectable, and a user checking their own history should be able to see the link
 * rather than take the app's word that it exists.
 *
 * @param entry the selected entry, or `null` before the user has chosen one.
 * @param devices the signed directory, for naming the signer.
 */
@Composable
fun AuditRecordScreen(
    entry: AuditEntry?,
    devices: Map<DeviceId, Device>,
    modifier: Modifier = Modifier,
) {
    if (entry == null) {
        CapsuleEmptyState(
            title = stringResource(R.string.android_audit_detail_title),
            modifier = modifier.fillMaxSize().testTag(AUDIT_RECORD_EMPTY_TAG),
            body = stringResource(R.string.android_audit_detail_empty),
        )
        return
    }
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag(AUDIT_RECORD_TAG),
    ) {
        CapsuleTopAppBar(
            title = auditEntryLabel(entry),
            subtitle = stringResource(R.string.android_audit_detail_title),
        )
        RecordField(stringResource(R.string.android_audit_field_when), formattedInstant(entry.at))
        RecordField(stringResource(R.string.android_audit_field_device), auditSignerLabel(entry, devices))
        entry.assetId?.let { asset ->
            RecordField(stringResource(R.string.android_audit_field_asset), asset.value)
        }
        when (entry) {
            is AuditEntry.Lifecycle -> LifecycleFields(entry)
            is AuditEntry.Moderation -> ModerationFields(entry)
        }
    }
}

@Composable
private fun LifecycleFields(entry: AuditEntry.Lifecycle) {
    val record = entry.record
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    CapsuleSectionHeader(title = stringResource(R.string.android_audit_field_record_hash))
    MonospaceValue(record.recordHash)
    CapsuleSectionHeader(title = stringResource(R.string.android_audit_field_prior_hash))
    val prior = record.priorProvenanceHash
    if (prior == null) {
        // Absent exactly on `create`, which is a fact about the chain rather than a gap in it.
        Text(
            text = stringResource(R.string.android_audit_chain_start),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(horizontal = PaneInset, vertical = FieldSpacing),
        )
    } else {
        MonospaceValue(prior)
    }
    record.retentionUntil?.let { until ->
        RecordField(stringResource(R.string.android_audit_field_retention_until), formattedInstant(until))
    }
    Text(
        text = stringResource(R.string.android_audit_chain_note),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(PaneInset),
    )
}

@Composable
private fun ModerationFields(entry: AuditEntry.Moderation) {
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    entry.event.reason?.let { reason ->
        RecordField(stringResource(R.string.android_audit_field_reason), reason)
    }
    Text(
        text = stringResource(R.string.android_audit_moderation_note),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(PaneInset),
    )
}

/** One labelled value. The label is catalogue copy; the value is data and never translated. */
@Composable
private fun RecordField(
    label: String,
    value: String,
) {
    CapsuleListItem(
        headline = value,
        modifier = Modifier.fillMaxWidth(),
        overline = label,
        contentDescription = label + SEPARATOR + value,
    )
}

/**
 * A hash, in the technical readout style.
 *
 * Hashes are compared by eye against another device's screen, and a proportional face makes
 * that unreliable in exactly the way that matters.
 */
@Composable
private fun MonospaceValue(value: String) {
    Text(
        text = value,
        style = CapsuleTheme.textStyles.metadata,
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(horizontal = PaneInset, vertical = FieldSpacing),
    )
}
