package com.jurisgames.sobres.ui

import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.ImageBitmap
import java.time.LocalDateTime
import java.time.ZoneId

/** Turns image bytes into a bitmap (per platform); kept for the shared core's image support. */
val LocalImageDecoder = staticCompositionLocalOf<((ByteArray) -> ImageBitmap?)?> { null }

fun fromMillis(ms: Long): LocalDateTime = LocalDateTime.ofInstant(java.time.Instant.ofEpochMilli(ms), ZoneId.systemDefault())
