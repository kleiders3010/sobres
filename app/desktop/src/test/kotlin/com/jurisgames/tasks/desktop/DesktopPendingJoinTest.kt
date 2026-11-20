package com.jurisgames.tasks.desktop

import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import org.junit.After
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * A terrible connection (owner's call, 2026-09-26): the code is entered while nothing can be
 * reached. The device keeps looking in later rounds (background sync on phones) and gets
 * in once the network is back, without the code being entered again.
 */
class DesktopPendingJoinTest {
    private val port = 18500 + (ProcessHandle.current().pid() % 300).toInt()
    private var relay: Process? = null

    @After
    fun stopRelay() {
        relay?.destroyForcibly()
    }

    private fun node(tag: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), false, false, "127.0.0.1:0", tag, "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun aCodeEnteredOfflineJoinsLater() {
        val a = node("inviter")
        a.setDisplayName("Alex")
        val g = a.createGroup("Casita")
        val invite = a.invite(g.id, 7u)
        val b = node("joiner")
        // No relay yet: nothing can be found.
        val r = b.joinCode(invite.link)
        check(r.stillLooking && !r.joined) { r.toString() }

        // The network comes back.
        relay = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
            it.inputStream.bufferedReader().readLine()
        }
        val joined = mutableListOf<String>()
        val deadline = System.currentTimeMillis() + 180_000
        while ("Casita" !in joined) {
            check(System.currentTimeMillis() < deadline) { "never got in; b has ${b.groups().map { it.name }}" }
            a.syncNow()
            joined += b.syncNow().joinedGroups
        }
        // In: the group's details follow with the next rounds.
        while (b.groups().none { it.name == "Casita" }) {
            check(System.currentTimeMillis() < deadline) { "in, but the group never showed: ${b.groups().map { it.name }}" }
            a.syncNow()
            b.syncNow()
        }
        a.close()
        b.close()
    }
}
