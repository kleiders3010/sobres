package com.jurisgames.sobres.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.foundation.selection.selectable
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.ListItem
import com.jurisgames.sobres.core.PersonItem
import com.jurisgames.sobres.core.ThemeColors
import com.jurisgames.sobres.core.accentColor
import com.jurisgames.sobres.core.iconSymbol
import com.jurisgames.sobres.core.listHues
import com.jurisgames.sobres.core.listIcons

/** A color's everyday name, for screen readers on the swatches (Tasks' U-T14, same bands). */
fun hueName(h: Double?): String {
    if (h == null) return tr("Grey")
    val x = ((h % 360) + 360) % 360
    return tr(when {
        x < 15 || x >= 345 -> "Red"
        x < 45 -> "Orange"
        x < 70 -> "Yellow"
        x < 160 -> "Green"
        x < 195 -> "Teal"
        x < 250 -> "Blue"
        x < 290 -> "Purple"
        else -> "Pink"
    })
}

/** The theme ID in use and dark mode, for list colors and avatars. */
data class Look(val themeId: String, val dark: Boolean)

val LocalLook = staticCompositionLocalOf { Look("default", false) }

/** Whether this platform offers wallpaper colors (Android 12+). */
val LocalDynamicAvailable = staticCompositionLocalOf { false }

fun argb(c: UInt) = Color(c.toInt())

private fun scheme(c: ThemeColors, dark: Boolean): ColorScheme {
    val base = if (dark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = argb(c.primary),
        onPrimary = argb(c.onPrimary),
        primaryContainer = argb(c.primaryContainer),
        onPrimaryContainer = argb(c.onPrimaryContainer),
        secondary = argb(c.secondary),
        onSecondary = argb(c.onSecondary),
        secondaryContainer = argb(c.secondaryContainer),
        onSecondaryContainer = argb(c.onSecondaryContainer),
        tertiary = argb(c.tertiary),
        onTertiary = argb(c.onTertiary),
        background = argb(c.background),
        onBackground = argb(c.onBackground),
        surface = argb(c.surface),
        onSurface = argb(c.onSurface),
        surfaceVariant = argb(c.surfaceVariant),
        onSurfaceVariant = argb(c.onSurfaceVariant),
        surfaceTint = argb(c.primary),
        error = argb(c.error),
        onError = argb(c.onError),
        outline = argb(c.outline),
        outlineVariant = argb(c.outlineVariant),
        surfaceContainer = argb(c.surfaceContainer),
        surfaceContainerHigh = argb(c.surfaceContainerHigh),
    )
}

/**
 * The device's theme (DESIGN §2 appearance): colors from the theme's tokens in the system's
 * light or dark mode (the platform wins), or wallpaper colors where chosen and available.
 */
@Composable
fun AppTheme(node: KtNode, version: Int, dynamicScheme: ((Boolean) -> ColorScheme?)?, content: @Composable () -> Unit) {
    val system = isSystemInDarkTheme()
    val mode = remember(node, version) { darkMode(node.dataDir()) }
    val dark = when (mode) { DarkMode.System -> system; DarkMode.Light -> false; DarkMode.Dark -> true }
    val settings = remember(node, version) { node.settings() }
    val colors = remember(node, version, dark) { node.themeColors(dark) }
    val cs = (if (settings.dynamicColor == true) dynamicScheme?.invoke(dark) else null) ?: scheme(colors, dark)
    val r = colors.cornerDp.toInt()
    val shapes = Shapes(small = RoundedCornerShape((r / 2).dp), medium = RoundedCornerShape(r.dp), large = RoundedCornerShape((r * 3 / 2).dp))
    CompositionLocalProvider(LocalLook provides Look(settings.theme, dark), LocalDynamicAvailable provides (dynamicScheme != null)) {
        MaterialTheme(colorScheme = cs, shapes = shapes, content = content)
    }
}

/** A list's color dot and icon. Color is only an accent, never the only signal. */
@Composable
fun ListBadge(l: ListItem, modifier: Modifier = Modifier) {
    val look = LocalLook.current
    val color = argb(accentColor(look.themeId, l.hue, l.vivid, look.dark))
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Box(Modifier.size(12.dp).background(color, CircleShape).testTag("dot-" + l.name))
        val icon = l.icon?.let { iconSymbol(it) }.orEmpty()
        if (icon.isNotEmpty()) Text(icon, modifier = Modifier.testTag("icon-" + l.name))
    }
}

/** A generated avatar: initials on the person's hue. */
@Composable
fun Avatar(p: PersonItem) {
    val look = LocalLook.current
    // U-S23: the circle grows with the text size, so initials never spill out at large text.
    val grow = androidx.compose.ui.platform.LocalDensity.current.fontScale.coerceIn(1f, 2f)
    Box(Modifier.size(28.dp * grow).background(argb(accentColor(look.themeId, p.hue, 1u.toUByte(), look.dark)), CircleShape), contentAlignment = Alignment.Center) {
        Text(p.initials, color = if (look.dark) Color.Black else Color.White, fontSize = 12.sp)
    }
}

/** A person's name, with the short tag when someone else in the list has the same name. */
fun personLabel(p: PersonItem, all: List<PersonItem>): String {
    // U-S11: the engine's fallback name is English.
    val name = if (p.name.isBlank() || p.name == "Someone") tr("Someone") else p.name
    val base = if (p.isMe) tr("{name} (you)", "name" to name) else name
    return if (all.count { it.name == p.name } > 1) "$base · ${p.tag}" else base
}

/** List color and icon (shared with the group): curated hues, grey, "More colors…", icons. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun StyleDialog(node: KtNode, list: ListItem, onDone: () -> Unit, onClose: () -> Unit) {
    val look = LocalLook.current
    var hue by remember { mutableStateOf(list.hue) }
    var vivid by remember { mutableStateOf(list.vivid) }
    var icon by remember { mutableStateOf(list.icon) }
    var more by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = { node.setListStyle(list.id, hue, vivid, icon); onDone(); onClose() }, modifier = Modifier.testTag("styleSave")) { Text(tr("Save")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("Color and icon")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    val choices: List<Double?> = listOf<Double?>(null) + listHues()
                    for (h in choices) {
                        val selected = hue == h
                        // a 48 dp target with a spoken name and state around the 32 dp dot (APP-RULES 140, 209)
                        Box(
                            Modifier.size(48.dp)
                                .selectable(selected, role = Role.RadioButton) { hue = h }
                                .semantics { contentDescription = hueName(h) }
                                .testTag("hue-" + (h?.toInt()?.toString() ?: "grey")),
                            contentAlignment = Alignment.Center,
                        ) {
                            Box(Modifier.size(32.dp)
                                .background(argb(accentColor(look.themeId, h, vivid, look.dark)), CircleShape)
                                .border(if (selected) 3.dp else 0.dp, MaterialTheme.colorScheme.onSurface, CircleShape))
                        }
                    }
                }
                TextButton(onClick = { more = !more }, modifier = Modifier.testTag("moreColors")) { Text(tr("More colors…")) }
                if (more) Slider((hue ?: 0.0).toFloat(), { hue = it.toDouble() }, valueRange = 0f..359f, modifier = Modifier.testTag("hueWheel"))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf(tr("Soft"), tr("Normal"), tr("Vivid")).forEachIndexed { i, name ->
                        FilterChip(vivid.toInt() == i, { vivid = i.toUByte() }, label = { Text(name) }, modifier = Modifier.testTag("vivid-$i"))
                    }
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    FilterChip(icon == null, { icon = null }, label = { Text(tr("None")) }, modifier = Modifier.testTag("icon-none"))
                    for (i in listIcons()) {
                        FilterChip(icon == i.id, { icon = i.id }, label = { Text(i.symbol) }, modifier = Modifier.testTag("pick-" + i.id))
                    }
                }
            }
        },
    )
}

/** Light or dark: the system's by default, or the person's own pick (cold UX review U-T11). This device only. */
enum class DarkMode(val key: String, val label: String) { System("system", "Same as the system"), Light("light", "Light"), Dark("dark", "Dark") }

fun darkMode(dataDir: String): DarkMode {
    val k = runCatching { java.io.File(dataDir, "dark_mode").readText().trim() }.getOrNull()
    return DarkMode.entries.firstOrNull { it.key == k } ?: DarkMode.System
}

fun setDarkMode(dataDir: String, m: DarkMode) {
    runCatching { java.io.File(dataDir, "dark_mode").writeText(m.key) }
}
