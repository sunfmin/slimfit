package com.example

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class CalcTest {
    @Test
    fun adds() {
        assertEquals(5, add(2, 3))
    }

    @Test
    fun addsAgain() {
        assertEquals(5, add(2, 3))
    }

    @Test
    fun adult() {
        assertTrue(isAdult(30))
    }

    @Test
    fun slow() {
        Thread.sleep(600)
        assertEquals(5, add(2, 3))
    }
}
