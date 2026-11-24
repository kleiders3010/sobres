package com.jurisgames.sobres.desktop

import com.jurisgames.sobres.ui.Feedback
import kotlin.test.Test
import kotlin.test.assertFalse

/**
 * Guard (Toma safety audit TM-5, 2026-11-24; APP-RULES 85): feedback names the system by its release, never
 * os.version (on Android the kernel build string, which narrows down the phone).
 */
class FeedbackSystemTest {
    @Test fun feedbackNeverSendsTheOsVersion() {
        val v = System.getProperty("os.version")
        val sent = Feedback.json("bug", "words", "", "1", "diagnostics line")
        assertFalse(v.isNotBlank() && sent.contains(v), "the report carries os.version ($v): $sent")
    }
}
