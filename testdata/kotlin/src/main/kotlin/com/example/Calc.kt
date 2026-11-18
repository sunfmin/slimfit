package com.example

fun add(a: Int, b: Int): Int = a + b

fun unused(x: Int): Int {
    val doubled = x * 2
    val shifted = doubled + 7
    return shifted - x
}

fun isAdult(age: Int): Boolean = age >= 18
