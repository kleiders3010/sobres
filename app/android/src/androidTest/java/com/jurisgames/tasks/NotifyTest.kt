package com.jurisgames.tasks

import android.app.NotificationManager
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.jurisgames.tasks.core.ChangeItem
import org.junit.Test
import org.junit.runner.RunWith

/** Change notifications are posted batched per list, with who and what. */
@RunWith(AndroidJUnit4::class)
class NotifyTest {
    @Test
    fun batchedPerList() {
        val inst = InstrumentationRegistry.getInstrumentation()
        val ctx = inst.targetContext
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            inst.uiAutomation.grantRuntimePermission(ctx.packageName, android.Manifest.permission.POST_NOTIFICATIONS)
        }
        val now = System.currentTimeMillis().toULong()
        fun c(list: String, who: String, kind: String, title: String) = ChangeItem("Family", "id-$list", list, who, kind, title, now)
        val kim = ChangeItem("Family", "", "", "Kim", "joined", "", now)
        ChangeNotifier.post(ctx, listOf(c("Groceries", "Sam", "added", "milk"), c("Groceries", "Sam", "added", "eggs"), c("Groceries", "Sam", "completed", "bread"), c("Trip", "Alex", "added", "passport"), kim))
        // Posting is asynchronous: wait until all three are showing.
        val nm = ctx.getSystemService(NotificationManager::class.java)
        fun texts() = nm.activeNotifications.associate { it.notification.extras.getString("android.title") to it.notification.extras.getCharSequence("android.text").toString() }
        val deadline = System.currentTimeMillis() + 10_000
        while (texts().size < 3 && System.currentTimeMillis() < deadline) Thread.sleep(100)
        val texts = texts()
        check(texts["Groceries"] == "Sam added milk, eggs and completed bread") { texts.toString() }
        check(texts["Trip"] == "Alex added passport") { texts.toString() }
        check(texts["Family"] == "Kim joined") { texts.toString() }
    }
}
