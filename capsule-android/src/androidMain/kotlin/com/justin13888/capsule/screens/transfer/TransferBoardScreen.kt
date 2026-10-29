package com.justin13888.capsule.screens.transfer

import android.content.Context
import android.text.format.Formatter
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.R
import com.justin13888.capsule.i18n.RegionalFormat
import com.justin13888.capsule.screens.domain.TransferRetryReason

/** Cells in each transfer's chunk strip. */
private const val STRIP_CELLS = 48

/** Hex digits of a content address shown to tell transfers apart. */
private const val BLOB_DIGITS = 12

/**
 * The real-time transfer view: one card per upload or fetch the board holds, with its
 * chunk strip, progress, speed, piece size, retries and custody. Material 3, the same
 * layout and feature set as the Apple client's transfer view.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TransferBoardScreen(
    viewModel: TransferBoardViewModel,
    navigateBack: () -> Unit,
) {
    val board by viewModel.board.collectAsStateWithLifecycle()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_transfer_board_title)) },
                navigationIcon = {
                    IconButton(onClick = navigateBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back))
                    }
                },
            )
        },
    ) { padding ->
        TransferBoardContent(board, Modifier.padding(padding))
    }
}

@Composable
private fun TransferBoardContent(
    board: TransferBoard,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    if (board.transfers.isEmpty()) {
        Column(
            modifier.fillMaxSize().padding(24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(stringResource(R.string.app_transfer_board_empty_title), style = MaterialTheme.typography.titleLarge)
            Text(stringResource(R.string.app_transfer_board_empty_description), style = MaterialTheme.typography.bodyMedium)
        }
        return
    }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(stringResource(R.string.app_transfer_board_total_speed, bytes(context, board.throughput)))
                Text(pluralStringResource(R.plurals.app_transfer_board_active, board.activeCount, board.activeCount))
            }
            if (board.missed > 0) {
                val missed = board.missed.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
                Text(
                    pluralStringResource(R.plurals.app_transfer_board_missed, missed, missed),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        items(board.transfers, key = { "${it.direction}:${it.blob.value}:${it.startedMilliseconds}" }) { transfer ->
            TransferCard(transfer, board.nowMilliseconds)
        }
        item {
            val number = RegionalFormat.number(context)
            Text(
                stringResource(
                    R.string.app_transfer_board_custody,
                    number.format(board.custody.durable),
                    number.format(board.custody.notDurable),
                    number.format(board.custody.receipts),
                ),
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun TransferCard(
    transfer: Transfer,
    now: Long,
) {
    val context = LocalContext.current
    val direction =
        stringResource(
            if (transfer.direction == Direction.UPLOAD) {
                R.string.app_transfer_board_direction_upload
            } else {
                R.string.app_transfer_board_direction_fetch
            },
        )
    val progress =
        transfer.size?.let { stringResource(R.string.app_transfer_board_progress, bytes(context, transfer.done), bytes(context, it)) }
            ?: stringResource(R.string.app_transfer_board_progress_unknown, bytes(context, transfer.done))
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(direction, style = MaterialTheme.typography.titleSmall)
                Text(stringResource(phaseText(transfer.phase)), style = MaterialTheme.typography.labelMedium)
            }
            Text(
                transfer.blob.value.take(BLOB_DIGITS),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            ChunkStrip(transfer, progress)
            Text(progress, style = MaterialTheme.typography.bodyMedium)
            TransferDetails(transfer, now)
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun TransferDetails(
    transfer: Transfer,
    now: Long,
) {
    val context = LocalContext.current
    val details =
        buildList {
            add(stringResource(R.string.app_transfer_board_speed, bytes(context, transfer.throughput(now))))
            transfer.chunkSize?.let { add(stringResource(R.string.app_transfer_board_chunk_size, bytes(context, it))) }
            if (transfer.chunkSizeChanges > 0) {
                add(pluralStringResource(R.plurals.app_transfer_board_resizes, transfer.chunkSizeChanges, transfer.chunkSizeChanges))
            }
            if (transfer.retries > 0) {
                add(pluralStringResource(R.plurals.app_transfer_board_retries, transfer.retries, transfer.retries))
            }
            if (transfer.rejects > 0) {
                add(pluralStringResource(R.plurals.app_transfer_board_rejects, transfer.rejects, transfer.rejects))
            }
        }
    FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        details.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
    if (transfer.held > 0) {
        Text(stringResource(R.string.app_transfer_board_held, bytes(context, transfer.held)), style = MaterialTheme.typography.bodySmall)
    }
    transfer.lastRetry?.let {
        Text(
            stringResource(retryText(it)),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** The strip of [Transfer.cells]: one coloured cell per slice of the blob. */
@Composable
private fun ChunkStrip(
    transfer: Transfer,
    description: String,
) {
    val cells = transfer.cells(STRIP_CELLS) ?: return
    val colors = MaterialTheme.colorScheme
    val palette =
        mapOf(
            Cell.PENDING to colors.surfaceVariant,
            Cell.HELD to colors.secondary,
            Cell.ACKED to colors.primary,
            Cell.ACKED_AFTER_RETRY to colors.tertiary,
            Cell.ACKED_AFTER_REJECT to colors.error.copy(alpha = 0.6f),
            Cell.VERIFIED to colors.primary,
            Cell.IN_FLIGHT to colors.primaryContainer,
            Cell.RETRYING to colors.tertiaryContainer,
            Cell.REJECTED to colors.error,
        )
    Canvas(
        Modifier
            .fillMaxWidth()
            .height(10.dp)
            .semantics { contentDescription = description },
    ) {
        val width = size.width / cells.size
        cells.forEachIndexed { index, cell ->
            drawRect(
                color = palette[cell] ?: Color.Transparent,
                topLeft = Offset(index * width, 0f),
                size = Size(maxOf(width - 1f, 1f), size.height),
            )
        }
    }
}

private fun phaseText(phase: Phase): Int =
    when (phase) {
        Phase.ACTIVE -> R.string.app_transfer_board_phase_active
        Phase.FINALIZING -> R.string.app_transfer_board_phase_finalizing
        Phase.VERIFIED -> R.string.app_transfer_board_phase_verified
        Phase.MERGED -> R.string.app_transfer_board_phase_merged
        Phase.INTEGRITY_REJECTED -> R.string.app_transfer_board_phase_integrity_rejected
    }

private fun retryText(note: RetryNote): Int =
    when (note) {
        is RetryNote.Upload ->
            when (note.reason) {
                is TransferRetryReason.TransportReset -> R.string.app_transfer_board_retry_transport_reset
                is TransferRetryReason.OffsetRealign -> R.string.app_transfer_board_retry_offset_realign
                TransferRetryReason.SessionLost -> R.string.app_transfer_board_retry_session_lost
                is TransferRetryReason.ServerWait -> R.string.app_transfer_board_retry_server_wait
            }
        is RetryNote.Fetch ->
            if (note.stalled) R.string.app_transfer_board_retry_fetch_stalled else R.string.app_transfer_board_retry_fetch_empty
    }

private fun bytes(
    context: Context,
    count: Long,
): String = Formatter.formatShortFileSize(context, count)
