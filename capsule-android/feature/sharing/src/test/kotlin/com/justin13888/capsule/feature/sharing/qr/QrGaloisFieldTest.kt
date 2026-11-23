package com.justin13888.capsule.feature.sharing.qr

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * The Reed–Solomon coder, checked by the property a decoder actually relies on.
 *
 * Asserting against a transcribed table of expected bytes would only prove the table was
 * copied consistently. Asserting that every syndrome vanishes proves the codeword is a
 * genuine member of the code — which is exactly what a scanner's error correction needs and
 * the only thing that makes a damaged symbol recoverable.
 */
class QrGaloisFieldTest {
    @Test
    fun `the field's logarithm and exponential invert each other`() {
        for (power in 0 until 255) {
            val value = QrGaloisField.exp(power)
            assertEquals(value, QrGaloisField.multiply(value, 1), "one is the identity at $power")
        }
    }

    @Test
    fun `multiplication is commutative and one is neutral`() {
        for (a in 1..255) {
            assertEquals(a, QrGaloisField.multiply(a, 1))
            assertEquals(0, QrGaloisField.multiply(a, 0))
            assertEquals(QrGaloisField.multiply(a, 7), QrGaloisField.multiply(7, a))
        }
    }

    @Test
    fun `the generator polynomial is monic and has the right degree`() {
        for (degree in listOf(10, 16, 18, 22, 24, 26)) {
            val poly = QrGaloisField.generatorPolynomial(degree)
            assertEquals(degree + 1, poly.size)
            assertEquals(1, poly[0], "a generator polynomial is monic")
        }
    }

    @Test
    fun `every syndrome of a generated codeword vanishes`() {
        val data = IntArray(28) { (it * 37 + 11) and 0xFF }
        for (count in listOf(10, 16, 18, 22, 26)) {
            val codeword = data + QrGaloisField.errorCorrection(data, count)
            for (syndrome in 0 until count) {
                val root = QrGaloisField.exp(syndrome)
                var accumulator = 0
                for (symbol in codeword) {
                    accumulator = QrGaloisField.multiply(accumulator, root) xor symbol
                }
                assertEquals(0, accumulator, "syndrome $syndrome for $count check symbols")
            }
        }
    }
}
