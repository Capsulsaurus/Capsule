package com.justin13888.capsule.feature.sharing.qr

/**
 * Arithmetic in GF(256) as QR's Reed–Solomon coder defines it.
 *
 * The field is fixed by the standard — primitive polynomial `x^8 + x^4 + x^3 + x^2 + 1`,
 * generator 2 — so the tables are computed once at class-init rather than being carried as
 * literals. A table that is derived cannot fall out of step with the polynomial beside it,
 * which a hand-transcribed one silently can.
 */
internal object QrGaloisField {
    /** The primitive polynomial, with its implicit `x^8` term. */
    private const val PRIMITIVE = 0x11D

    /** Field size, and the multiplicative order of the generator. */
    private const val ORDER = 255

    private val exponentials = IntArray(2 * ORDER)
    private val logarithms = IntArray(ORDER + 1)

    init {
        var value = 1
        for (power in 0 until ORDER) {
            exponentials[power] = value
            logarithms[value] = power
            value = value shl 1
            if (value and 0x100 != 0) value = value xor PRIMITIVE
        }
        // The doubled tail lets a product's exponent be added without a modulo per multiply.
        for (power in ORDER until 2 * ORDER) {
            exponentials[power] = exponentials[power - ORDER]
        }
    }

    /** The generator raised to [power]. */
    fun exp(power: Int): Int = exponentials[power % ORDER]

    /** The field product of [a] and [b]. */
    fun multiply(
        a: Int,
        b: Int,
    ): Int = if (a == 0 || b == 0) 0 else exponentials[logarithms[a] + logarithms[b]]

    /**
     * The Reed–Solomon generator polynomial of [degree], highest term first.
     *
     * Built by repeatedly multiplying by `(x - α^i)`; subtraction and addition are the same
     * operation in a field of characteristic two, which is why the sign never appears.
     */
    fun generatorPolynomial(degree: Int): IntArray {
        var poly = intArrayOf(1)
        for (i in 0 until degree) {
            val next = IntArray(poly.size + 1)
            for (j in poly.indices) {
                next[j] = next[j] xor poly[j]
                next[j + 1] = next[j + 1] xor multiply(poly[j], exp(i))
            }
            poly = next
        }
        return poly
    }

    /**
     * The [count] error-correction codewords for one block of [data].
     *
     * Polynomial long division of `data · x^count` by the generator, keeping the remainder.
     * The remainder is what the decoder's syndromes vanish on, which is the property the
     * tests assert rather than a table of expected bytes.
     */
    fun errorCorrection(
        data: IntArray,
        count: Int,
    ): IntArray {
        val generator = generatorPolynomial(count)
        val remainder = IntArray(count)
        for (byte in data) {
            val factor = byte xor remainder[0]
            for (i in 0 until count - 1) remainder[i] = remainder[i + 1]
            remainder[count - 1] = 0
            for (i in 0 until count) {
                remainder[i] = remainder[i] xor multiply(generator[i + 1], factor)
            }
        }
        return remainder
    }
}
