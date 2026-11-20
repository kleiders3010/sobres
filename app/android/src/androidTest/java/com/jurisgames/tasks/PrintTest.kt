package com.jurisgames.tasks

import android.content.Context
import android.print.PrintManager
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.jurisgames.tasks.ui.PrintChoice
import com.jurisgames.tasks.ui.Sort
import com.jurisgames.tasks.ui.printHtml
import org.junit.Test
import org.junit.runner.RunWith

/** Print list reaches the phone's print service: a print job named after the list (the system's preview opens). */
@RunWith(AndroidJUnit4::class)
class PrintTest {
    @Test
    fun printListOpensThePrintService() {
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        val node = (ctx.applicationContext as TasksApplication).node
        val list = node.createList(null, "Print me")
        val t = node.addTask(list.id, "Pack the bags", null)
        node.addTask(list.id, "passport", t)
        val html = printHtml(list.name, node.tasks(list.id), Sort.Order, PrintChoice())
        ActivityScenario.launch(MainActivity::class.java).use { sc ->
            var pm: PrintManager? = null
            sc.onActivity { a ->
                pm = a.getSystemService(Context.PRINT_SERVICE) as PrintManager
                Printing.print(a, "Print me", html)
            }
            val until = System.currentTimeMillis() + 15_000
            var names = emptyList<String>()
            while (System.currentTimeMillis() < until) {
                InstrumentationRegistry.getInstrumentation().runOnMainSync { names = pm!!.printJobs.map { it.info.label } }
                if ("Print me" in names) break
                Thread.sleep(250)
            }
            check("Print me" in names) { "no print job: $names" }
            Thread.sleep(InstrumentationRegistry.getArguments().getString("hold")?.toLongOrNull() ?: 0L)
        }
    }
}
