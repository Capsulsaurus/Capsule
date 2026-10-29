package com.justin13888.capsule.screens.transfer

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.screens.domain.TransferEventPort
import com.justin13888.capsule.screens.domain.TransferFeedItem
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.util.logging.Logger

/**
 * Holds the [TransferBoard] the transfer view draws: subscribes to the port's stream
 * when created, folds every item in, and ticks the board's clock so throughput decays
 * while nothing moves. Cancelling the ViewModel ends the subscription and nothing else;
 * the transfers never wait on this observer.
 *
 * [clock] is a monotonic millisecond clock; only the elapsed time since the subscription
 * opened is read from it.
 */
class TransferBoardViewModel(
    port: TransferEventPort,
    clock: () -> Long,
) : ViewModel() {
    private val mutableBoard = MutableStateFlow(TransferBoard())

    /** The board as of the latest item or tick. */
    val board: StateFlow<TransferBoard> = mutableBoard.asStateFlow()

    init {
        val opened = clock()
        val elapsed = { clock() - opened }
        log.info("transfer board subscribed")
        viewModelScope.launch {
            port.events().collect { item ->
                val wait = pacingDelay(item, elapsed())
                if (wait > 0) delay(wait)
                if (item is TransferFeedItem.Lagged) log.info("transfer board missed ${item.count} events")
                mutableBoard.update { it.apply(item) }
            }
            log.fine("transfer event stream ended")
        }
        viewModelScope.launch {
            while (isActive) {
                delay(TICK_MILLISECONDS)
                mutableBoard.update { it.tick(elapsed()) }
            }
        }
    }

    override fun onCleared() {
        log.info("transfer board unsubscribed")
    }

    companion object {
        /** How often the board's clock moves between events. */
        const val TICK_MILLISECONDS = 250L

        private val log: Logger = Logger.getLogger("capsule.transfer")

        /**
         * How long to hold [item] so it lands when its stamp says, given [elapsed]
         * milliseconds since the subscription opened. A live stream is stamped on
         * arrival, so this is zero for it; a recorded one replays at its own pace.
         */
        fun pacingDelay(
            item: TransferFeedItem,
            elapsed: Long,
        ): Long =
            when (item) {
                is TransferFeedItem.Event -> maxOf(0L, item.stamped.atMilliseconds - elapsed)
                is TransferFeedItem.Lagged -> 0L
            }
    }
}
