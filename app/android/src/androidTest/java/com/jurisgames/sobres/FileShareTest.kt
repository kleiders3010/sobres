package com.jurisgames.sobres

import androidx.core.content.FileProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** Exported .tasklist files can be handed to other apps (FileProvider set up right). */
@RunWith(AndroidJUnit4::class)
class FileShareTest {
    @Test
    fun exportedListsGetAShareableLink() {
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        val node = (ctx.applicationContext as TasksApplication).node
        val list = node.createList(null, "Shared out")
        val dir = File(ctx.cacheDir, "exports").apply { mkdirs() }
        val file = File(dir, "Shared out.tasklist").apply { writeBytes(node.exportListFile(list.id)) }
        val uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", file)
        check(uri.scheme == "content") { uri.toString() }
        val back = ctx.contentResolver.openInputStream(uri)!!.use { it.readBytes() }
        check(back.contentEquals(file.readBytes()))
        // What the other app would import:
        check(node.importListFile(null, back).name == "Shared out")
    }
}
