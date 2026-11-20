//! Readable exports (DESIGN §11).

use crate::TaskView;

/// Markdown in the Obsidian Tasks format: `- [ ]` / `- [x]` items, subtasks indented with
/// tabs, notes as indented lines under their task. One-way: nothing reads this back.
pub fn markdown(list_name: &str, tasks: &[TaskView]) -> String {
    let mut out = format!("# {}\n\n", one_line(list_name));
    items(tasks, 0, &mut out);
    out
}

fn items(ts: &[TaskView], depth: usize, out: &mut String) {
    for t in ts {
        let indent = "\t".repeat(depth);
        out.push_str(&format!("{indent}- [{}] {}\n", if t.done { 'x' } else { ' ' }, one_line(&t.title)));
        for line in t.notes_preview().lines().filter(|l| !l.trim().is_empty()) {
            out.push_str(&format!("{indent}  {}\n", line.trim_end()));
        }
        if t.attachment().is_some() {
            out.push_str(&format!("{indent}  (continues in the app)\n"));
        }
        items(&t.children, depth + 1, out);
    }
}

/// Titles are one line in Markdown; an empty title would read as an empty checkbox.
fn one_line(s: &str) -> String {
    let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() { "(untitled)".into() } else { t }
}

/// A safe file name stem for exporting a list (D60): list titles come from other devices,
/// so only letters, digits, spaces and `-_.()` survive, never a path separator, a leading
/// dot, a trailing dot or space, or a Windows device name, at most 60 characters. The
/// caller adds the extension and joins it to a folder it chose.
pub fn file_stem(title: &str) -> String {
    let kept: String = title
        .chars()
        .take(200)
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') { c } else { '_' })
        .collect();
    let spaced = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = spaced.trim_matches(['.', ' ']).to_string();
    if is_device_name(&s) {
        s.insert(0, '_');
    }
    // The length cap last, so nothing added above can exceed it.
    let s: String = s.chars().take(60).collect();
    let s = s.trim_end_matches(['.', ' ']);
    if s.is_empty() { "list".into() } else { s.into() }
}

/// Windows device names (`CON`, `COM1`, `LPT²`...), which also apply with any extension.
pub fn is_device_name(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let rest: Vec<char> = base.chars().skip(3).collect();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT")) && rest.len() == 1 && rest[0].is_numeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_stems() {
        assert_eq!(file_stem("Groceries"), "Groceries");
        assert_eq!(file_stem("Café & Bar"), "Café _ Bar");
        assert_eq!(file_stem("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(file_stem("..\\..\\x"), "_.._x");
        assert_eq!(file_stem(".hidden"), "hidden");
        assert_eq!(file_stem("con"), "_con");
        assert_eq!(file_stem("LPT1.txt"), "_LPT1.txt");
        assert_eq!(file_stem("com¹"), "_com¹");
        assert_eq!(file_stem("Computer"), "Computer");
        assert_eq!(file_stem("   "), "list");
        assert_eq!(file_stem("a\nb\u{202e}c"), "a_b_c");
        assert_eq!(file_stem(&"x".repeat(200)).len(), 60);
    }

    fn t(title: &str, done: bool, notes: &str, children: Vec<TaskView>) -> TaskView {
        TaskView { id: String::new(), title: title.into(), done, notes: notes.into(), remind_ms: None, repeat: None, starred_ms: None, my_day: vec![], assignee: None, images: vec![], children }
    }

    #[test]
    fn obsidian_tasks_format() {
        let md = markdown("Groceries", &[t("milk", true, "", vec![]), t("party", false, "Saturday\n\nbring chairs", vec![t("cake\nchocolate", false, "", vec![])])]);
        assert_eq!(md, "# Groceries\n\n- [x] milk\n- [ ] party\n  Saturday\n  bring chairs\n\t- [ ] cake chocolate\n");
    }
}
