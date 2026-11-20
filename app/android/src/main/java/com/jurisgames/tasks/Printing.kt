package com.jurisgames.tasks

import android.app.Activity
import android.content.Context
import android.print.PrintAttributes
import android.print.PrintManager
import android.webkit.WebView
import android.webkit.WebViewClient

/**
 * Print list on Android: the page is laid out in an off-screen WebView and handed to the phone's print service
 * (a printer, or Save as PDF). The WebView is kept until its page has gone to the PrintManager (Android's
 * "Printing HTML documents" guide: it must not be collected before then).
 */
object Printing {
    private var pending: WebView? = null

    fun print(activity: Activity, name: String, html: String) {
        val view = WebView(activity)
        view.webViewClient = object : WebViewClient() {
            override fun onPageFinished(v: WebView, url: String?) {
                if (pending !== v) return // already handed over (onPageFinished can come twice)
                pending = null
                val pm = activity.getSystemService(Context.PRINT_SERVICE) as PrintManager
                pm.print(name, v.createPrintDocumentAdapter(name), PrintAttributes.Builder().build())
            }
        }
        pending = view
        view.loadDataWithBaseURL(null, html, "text/html", "utf-8", null)
    }
}
