package com.justin13888.capsule.screens.culling

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals

/** Writes reach the store one at a time, in submission order, even when an earlier one is slower. */
class WriteQueueTest {
    @Test
    fun writesRunInSubmissionOrderEvenWhenTheFirstIsSlow() =
        runBlocking {
            val queue = WriteQueue()
            val seen = mutableListOf<String>()
            queue.submit {
                delay(50)
                seen += "reject"
            }
            queue.submit { seen += "neutral" }
            queue.close()

            queue.run()

            assertEquals(listOf("reject", "neutral"), seen)
        }
}
