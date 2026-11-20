package com.jurisgames.tasks.desktop

import java.io.File

/**
 * "Start with computer" (DESIGN settings, on by default): the packaged app registers itself
 * to start in the tray at login. Windows: the per-user `Run` registry key (no admin, no
 * prompt). Linux: an XDG autostart entry. Never registers a development JVM.
 */
object Autostart {
    private const val NAME = "JurisTasks"
    const val BACKGROUND_ARG = "--background"

    private val isWindows get() = System.getProperty("os.name").lowercase().contains("win")

    /** The packaged app's launcher, or null when running from a plain JVM (tests, gradle run). */
    fun launcher(command: String? = ProcessHandle.current().info().command().orElse(null)): String? {
        val exe = command ?: return null
        val name = File(exe).name.lowercase()
        return if (name.startsWith("java")) null else exe
    }

    /** The line the system runs at login. */
    fun commandLine(launcher: String): String = "\"$launcher\" $BACKGROUND_ARG"

    /** The XDG autostart entry (Linux). */
    fun desktopEntry(launcher: String): String = """
        |[Desktop Entry]
        |Type=Application
        |Name=Tasks
        |Exec=${commandLine(launcher)}
        |X-GNOME-Autostart-enabled=true
        |NoDisplay=true
        |""".trimMargin()

    private fun linuxEntry(): File {
        val config = System.getenv("XDG_CONFIG_HOME")?.let { File(it) } ?: File(System.getProperty("user.home"), ".config")
        return File(config, "autostart/juristasks.desktop")
    }

    /** Make the system match the setting. Returns false when there's nothing to register. */
    fun apply(enabled: Boolean, launcher: String? = launcher()): Boolean {
        val exe = launcher ?: return false
        if (isWindows) {
            val key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run"
            val cmd = if (enabled) listOf("reg", "add", key, "/v", NAME, "/t", "REG_SZ", "/d", commandLine(exe), "/f")
            else listOf("reg", "delete", key, "/v", NAME, "/f")
            return runCatching { ProcessBuilder(cmd).redirectErrorStream(true).start().waitFor() == 0 }.getOrDefault(false)
        }
        val entry = linuxEntry()
        return runCatching {
            if (enabled) {
                entry.parentFile.mkdirs()
                entry.writeText(desktopEntry(exe))
            } else {
                entry.delete()
            }
            true
        }.getOrDefault(false)
    }
}
