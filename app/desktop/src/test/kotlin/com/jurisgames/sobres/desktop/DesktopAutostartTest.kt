package com.jurisgames.sobres.desktop

import org.junit.Test

/** "Start with computer": what gets registered (without touching this machine's settings). */
class DesktopAutostartTest {
    @Test
    fun onlyThePackagedAppRegisters() {
        check(Autostart.launcher(null) == null)
        check(Autostart.launcher("C:\\Program Files\\Java\\bin\\java.exe") == null) { "never a development JVM" }
        check(Autostart.launcher("/usr/lib/jvm/bin/java") == null)
        check(Autostart.launcher("C:\\Program Files\\Tasks\\Tasks.exe") == "C:\\Program Files\\Tasks\\Tasks.exe")
        check(Autostart.apply(enabled = true, launcher = null) == false) { "nothing to register from a JVM" }
    }

    @Test
    fun startsInTheTray() {
        check(Autostart.commandLine("C:\\Program Files\\Tasks\\Tasks.exe") == "\"C:\\Program Files\\Tasks\\Tasks.exe\" --background")
        val entry = Autostart.desktopEntry("/opt/tasks/bin/Tasks")
        check(entry.contains("Exec=\"/opt/tasks/bin/Tasks\" --background")) { entry }
        check(entry.startsWith("[Desktop Entry]"))
    }
}
