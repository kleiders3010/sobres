package com.jurisgames.tasks.desktop

import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * The desktop app tells the update manifest which build it is. It must be the same number and name as the Android
 * build of the same release, or desktop users are told about an update they already have (1.0.1 shipped saying 1).
 */
class VersionCodeTest {
    private fun find(file: String, pattern: String): String {
        val f = listOf(File(file), File("app/$file"), File("../$file")).first { it.exists() }
        return Regex(pattern).find(f.readText())!!.groupValues[1]
    }

    @Test
    fun desktopVersionMatchesAndroid() {
        assertEquals(find("android/build.gradle.kts", """versionCode\s*=\s*(\d+)""").toULong(), VERSION_CODE)
        assertEquals(
            find("android/build.gradle.kts", """versionName\s*=\s*"([^"]+)""""),
            find("desktop/build.gradle.kts", """packageVersion\s*=\s*"([^"]+)""""),
        )
    }
}
