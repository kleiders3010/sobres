# Tasks: shared lists, no account

A free, open-source to-do and shopping list app for Android and Windows. Open it and type: it starts on your last
list. Share a list with a link, QR code or 8 words; changes reach everyone's devices end-to-end encrypted, with no
account and no company server holding your data.

- **Quick:** Enter adds and keeps the field open; "tomorrow at 5" becomes a reminder and "every monday" a repeating one
  (ticked off, it comes back on its next date, like Todoist's); shopping lists bring finished
  items back with one tap; swipe to delete with Undo.
- **Starred:** star what matters (like Google Tasks): every starred task of every list in one place, and each list
  sorted by your order, by date, or starred first.
- **Upcoming:** every task with a date, from every list, under its day (like Todoist's Today and Upcoming);
  overdue ones move to today in one tap.
- **Print:** print a list (like Microsoft To Do's Print list), with or without its steps and notes: what's left to
  do, in the list's order, with a box to tick on paper; long names wrap instead of being cut off. Android's print
  service (a printer or Save as PDF) or the Windows print dialog.
- **Shared:** any list can be shared; everyone edits at once and edits merge (CRDTs, [Loro](https://loro.dev)).
  In a shared list a task can be assigned to one of its people, yourself too (like Microsoft To Do's Assign to):
  the row shows who, and "Assigned to me" gathers what's yours from every list. Moving a task to another list
  drops its assignment.
- **No servers you depend on:** devices reach each other directly ([iroh](https://iroh.computer)), over the same
  Wi-Fi, through public Nostr relays, the Mainline DHT, other users' apps that help carry changes, and a small mailbox.
  Every layer only ever sees encrypted data. If one disappears, the others keep working.

## Download
- Android: [Google Play](https://play.google.com/store/apps/details?id=com.jurisgames.tasks) or the APK on the
  [releases page](https://github.com/kleiders3010/jurisgames/releases)
- Windows: installer on the [releases page](https://github.com/kleiders3010/jurisgames/releases)

## Build
- Rust core (`crates/`): `cargo test --workspace`
- Android: `scripts/build-android-libs.sh`, then `cd app && ./gradlew :android:assemblePlayRelease`
- Desktop: `scripts/package-desktop.sh`
- Design: [docs/DESIGN.md](docs/DESIGN.md) · UX: [docs/UX-PRINCIPLES.md](docs/UX-PRINCIPLES.md),
  [docs/UI-SPEC.md](docs/UI-SPEC.md)

## License
Apache-2.0. Icons from Google's Material Icons (Apache-2.0).
