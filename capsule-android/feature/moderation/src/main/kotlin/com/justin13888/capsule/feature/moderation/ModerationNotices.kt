package com.justin13888.capsule.feature.moderation

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventKind
import com.justin13888.capsule.core.model.knownOrNull

/** Test handle for one notice card: `moderation.notice:<id>`. */
fun moderationNoticeTag(eventId: String): String = "moderation.notice:$eventId"

/**
 * What one entry of the moderation record says happened.
 *
 * A token this build does not know is named as such rather than hidden. The set is closed and
 * versioned, so an unknown token means a newer client wrote the entry — and dropping the row
 * would tell the user nothing happened, which is the one thing this record must never say.
 */
@Composable
internal fun moderationKindLabel(event: ModerationEvent): String =
    when (event.kind.knownOrNull) {
        ModerationEventKind.Takedown -> stringResource(R.string.android_audit_moderation_takedown)
        ModerationEventKind.TakedownLifted -> stringResource(R.string.android_audit_moderation_takedown_lifted)
        ModerationEventKind.LegalHold -> stringResource(R.string.android_audit_moderation_legal_hold)
        ModerationEventKind.Suspended -> stringResource(R.string.android_audit_moderation_suspended)
        ModerationEventKind.Unsuspended -> stringResource(R.string.android_audit_moderation_unsuspended)
        null -> stringResource(R.string.android_moderation_notice_unknown)
    }

/**
 * One entry of the moderation record, with what it did and did not do.
 *
 * The consequence copy is the point of this card. A takedown constrains what the *home
 * server* hands out; it destroys nothing, and the user's own backup still restores the asset.
 * Saying so plainly is the difference between a notice and a bereavement — and it is also
 * simply true, which is why the wording never hedges toward "removed".
 *
 * @param event the record entry to render.
 * @param onAppeal opens the appeal flow for this entry, or `null` where one is already open.
 */
@Composable
internal fun ModerationNoticeCard(
    event: ModerationEvent,
    modifier: Modifier = Modifier,
    onAppeal: (() -> Unit)? = null,
) {
    val kind = moderationKindLabel(event)
    val at = formattedInstant(event.createdAt)
    CapsuleCard(
        modifier = modifier.fillMaxWidth().testTag(moderationNoticeTag(event.id.value)),
        onClick = onAppeal,
        contentDescription = kind + SEPARATOR + at,
    ) {
        Text(text = kind, style = MaterialTheme.typography.titleMedium)
        Text(
            text = at,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        event.assetId?.let { asset ->
            Text(
                text = stringResource(R.string.android_moderation_takedown_asset).withArguments("id" to asset.value),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        NoticeConsequences(event)
        Text(
            text =
                event.reason
                    ?.takeIf(String::isNotBlank)
                    ?.let { stringResource(R.string.android_moderation_notice_reason).withArguments("reason" to it) }
                    ?: stringResource(R.string.android_moderation_notice_no_reason),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = ScreenSpacing),
        )
    }
}

/**
 * What this kind of action actually constrains.
 *
 * Only the two asset-level kinds get consequence copy; a suspension's effects are rendered
 * beside the banner instead, where they belong to the account rather than to one entry.
 */
@Composable
private fun NoticeConsequences(event: ModerationEvent) {
    val body =
        when (event.kind.knownOrNull) {
            ModerationEventKind.Takedown -> R.string.android_moderation_takedown_body
            ModerationEventKind.LegalHold -> R.string.android_moderation_takedown_legal_hold
            else -> null
        } ?: return
    Column(modifier = Modifier.padding(top = ScreenSpacing)) {
        Text(text = stringResource(body), style = MaterialTheme.typography.bodyMedium)
        if (event.kind.knownOrNull == ModerationEventKind.Takedown) {
            Text(
                text = stringResource(R.string.android_moderation_takedown_reversible),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
