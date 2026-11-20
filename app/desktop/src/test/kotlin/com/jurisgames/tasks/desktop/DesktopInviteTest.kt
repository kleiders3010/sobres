package com.jurisgames.tasks.desktop

import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import org.junit.After
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * The inviter shows a code and quits right away (found on a real phone, 2026-09-25): the
 * invite must already be out, so the joiner finds it with the inviter gone. Direct
 * connections off, so it can only come through the (local loopback) relay.
 */
class DesktopInviteTest {
    private val port = 18200 + (ProcessHandle.current().pid() % 300).toInt()
    private val relay: Process = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
        it.inputStream.bufferedReader().readLine()
    }

    @After
    fun stopRelay() {
        relay.destroyForcibly()
    }

    private fun node(tag: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), false, false, "127.0.0.1:0", tag, "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun anInviteIsFoundAfterTheInviterQuits() {
        val a = node("inviter")
        a.setDisplayName("Alex")
        val g = a.createGroup("Casita")
        val invite = a.invite(g.id, 7u)
        // Quit as the tray menu does: a bounded flush, then gone.
        val started = System.currentTimeMillis()
        a.stopLive()
        a.flush(10_000uL)
        check(System.currentTimeMillis() - started < 12_000) { "quitting is bounded" }
        a.close()

        val b = node("joiner")
        val r = b.joinCode(invite.link)
        check(r.groupName == "Casita" && r.inviterName == "Alex") { r.toString() }
        b.close()
    }
}
