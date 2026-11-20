package com.jurisgames.tasks.desktop

import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import org.junit.After
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * A text attachment reaches another member through the normal sync rounds: the task (with
 * its preview) arrives with the list, then the blob from the author's device. Two nodes in
 * this process, a local loopback relay; nothing leaves this machine.
 */
class DesktopAttachmentSyncTest {
    private val port = 17900 + (ProcessHandle.current().pid() % 300).toInt()
    private val relay: Process = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
        it.inputStream.bufferedReader().readLine()
    }

    @After
    fun stopRelay() {
        relay.destroyForcibly()
    }

    private fun node(tag: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), true, false, "127.0.0.1:0", tag, "") // P2P on: blobs travel directly
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun attachmentArrivesAtAnotherMember() {
        val a = node("alex")
        val b = node("sam")
        a.setDisplayName("Alex")
        b.setDisplayName("Sam")
        val g = a.createGroup("Trip")
        val list = a.createList(g.id, "Plans")
        val task = a.addTask(list.id, "Itinerary", null)
        val long = "Day by day, with every train and every address. ".repeat(3000) // ~150 KB
        a.setNotes(list.id, task, long)
        val invite = a.invite(g.id, 7u)
        a.syncNow()
        b.joinCode(invite.link)

        val deadline = System.currentTimeMillis() + 180_000
        fun bNotes() = b.lists().find { it.name == "Plans" }?.let { l -> b.tasks(l.id).find { it.title == "Itinerary" }?.let { b.notes(l.id, it.id) } }
        while (bNotes()?.complete != true) {
            check(System.currentTimeMillis() < deadline) { "attachment didn't arrive: ${bNotes()?.let { "preview ${it.text.length} of ${it.size}" } ?: "no task yet"}" }
            a.syncNow()
            b.syncNow()
        }
        check(bNotes()!!.text == long)
        a.close()
        b.close()
    }
}
