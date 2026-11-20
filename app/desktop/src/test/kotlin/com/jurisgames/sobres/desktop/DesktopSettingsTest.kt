package com.jurisgames.sobres.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.SobresApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** The settings screen shows exactly this platform's settings and keeps what's saved. */
class DesktopSettingsTest {
    @get:Rule
    val rule = createComposeRule()

    private fun node(home: File, platform: Platform): KtNode {
        val options = NodeOptions(platform, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun desktopSettingsAndTheyStick() {
        val home = Files.createTempDirectory("kt-settings").toFile()
        val n = node(home, Platform.DESKTOP)
        rule.setContent { SobresApp(n) }
        rule.onNodeWithTag("settingsOpen").performClick()
        rule.waitUntil(5_000) { shown("set-notify") }
        check(shown("set-relay")) { "relay setting on desktop (D51)" }
        check(shown("set-autostart"))
        check(shown("help-on") && shown("help-off") && !shown("help-wifi_and_charging"))
        rule.onNodeWithTag("set-notify").performClick()
        rule.onNodeWithTag("help-off").performClick()
        rule.onNodeWithTag("settingsSave").performClick()
        rule.waitUntil(5_000) { !shown("settingsSave") }
        n.close()
        val again = node(home, Platform.DESKTOP).settings()
        check(!again.notifyChanges && again.helpOthers == "off" && again.relayForOthers == true) { again.toString() }
    }

    @Test
    fun phonesHaveNoRelayOrAutostartSetting() {
        val n = node(Files.createTempDirectory("kt-settings-phone").toFile(), Platform.PHONE)
        rule.setContent { SobresApp(n) }
        rule.onNodeWithTag("settingsOpen").performClick()
        rule.waitUntil(5_000) { shown("set-notify") }
        check(!shown("set-relay")) { "not shown at all on phones (D51)" }
        check(!shown("set-autostart"))
        check(shown("help-wifi_and_charging") && shown("help-wifi_while_open") && shown("help-off") && !shown("help-on"))
        val s = n.settings()
        check(runCatching { n.setSettings(s.copy(relayForOthers = true)) }.isFailure) { "refused on phones" }
    }
}
