package com.jurisgames.tasks

import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.glance.appwidget.testing.unit.runGlanceAppWidgetUnitTest
import androidx.glance.testing.unit.hasTestTag
import androidx.glance.testing.unit.hasText
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.runner.RunWith

/** The widget's content: title, absolute update time, tasks with checkboxes. */
@RunWith(AndroidJUnit4::class)
class WidgetTest {
    @Test
    fun showsTheList() = runGlanceAppWidgetUnitTest {
        setContext(InstrumentationRegistry.getInstrumentation().targetContext)
        setAppWidgetSize(DpSize(250.dp, 200.dp))
        val state = WidgetState("l1", "Groceries", listOf(WidgetTask("a", "milk", false, 0), WidgetTask("b", "bread", true, 0), WidgetTask("c", "oat milk", false, 1)), 0)
        provideComposable { WidgetContent(state) }
        onNode(hasTestTag("widgetTitle")).assert(hasText("Groceries"))
        onNode(hasTestTag("wtask-milk")).assertExists()
        onNode(hasTestTag("wtask-bread")).assertExists()
        onNode(hasTestTag("wtask-oat milk")).assertExists()
        onNode(hasText("Updated")).assertExists() // hasText matches a substring
    }
}
