# UI spec: "best of all worlds" (2026-11-18)

Goal from the owner: *simple to understand, simple to use, and QUICK to use, meaning everything you need is
accessible*. Built against the competitors' real Play Store screenshots
(downloaded 2026-11-18, kept out of this repo since they are other companies' material), not from memory. UX-PRINCIPLES.md and UX-NOTES.md still hold.

## What each competitor does well (seen in their screenshots)

| App | Kept | Left out |
|---|---|---|
| Google Tasks | Lists as tabs across the top; "Completed (4)" folded at the bottom; new task in a bottom sheet with 3 small icons (details, date, star) above the keyboard | Gmail / Calendar tie-ins |
| Microsoft To Do | Big list title in the list's own colour; shared list shows people's avatars in the header; list colour / photo theme picker | My Day, flagged email |
| Todoist | Quick add that understands "tomorrow 4pm" and shows it as a chip while typing | Priorities p1-p4, labels, boards, projects-in-projects |
| TickTick | Time of a task shown right-aligned on its row | Matrix, pomodoro, habits, calendar |
| Any.do | Input "I want to…" always visible at the bottom of the list | Today/Tomorrow/Upcoming sections, WhatsApp |
| Bring! | "Recently used" items come back with one tap in shopping lists | Item tile grid, recipes, catalog images |
| OurGroceries | Plain rows, checked items leave the way | Aisle categories (needs a catalog) |
| Keep | Checklist items drop to the bottom when ticked | Notes board |
| Tasks.org (open source) | Everything works offline, no account | Dense settings, drawer of filters, priority dots |

## The app in one screen

Opening the app shows **the last list you used, ready to type into**. There is no home menu to pass first.

```
┌───────────────────────────────────────┐
│ ☰  Groceries                 (A)(U) ⋮ │  title in the list's colour; avatars = shared with
│ [Groceries] [To do] [Work] [+ New list]│  list chips, scroll sideways (phone)
│───────────────────────────────────────│
│ ○ Milk                                 │
│ ○ Eggs                     tomorrow 9:00│  reminder right-aligned (TickTick)
│   ○ free range                         │  subtask, indented
│ ○ Bread                          🗒 ▢  │  small marks: has notes / has a picture
│                                         │
│ ▸ Completed (3)                         │  folded (Google Tasks)
│───────────────────────────────────────│
│ [ Add an item…                       ] │  always there (Any.do), Enter adds and stays open
└───────────────────────────────────────┘
```

Wide screens (desktop, tablets ≥ 720 dp): the chips become a **sidebar** of lists on the left, the list on the right.

### Rows: one tap does the obvious thing
- **Tap the circle**: done. In every list the item drops into "Completed" after a 1 s pause (so a mis-tap is
  visible and can be tapped back). A snackbar "Done · Undo" for 4 s.
- **Tap the text**: opens the item's **details sheet** (below). No inline buttons on rows: the prototype had six.
- **Swipe** left or right: delete, with "Deleted · Undo" (principle: undo, not "are you sure").
- **Long-press and drag**: reorder (and on desktop, drag anywhere on the row).

### Adding: the fastest path
- The add field is always visible at the bottom. **Enter adds and keeps the keyboard open** for the next one.
- **When** words are understood while typing, and shown as a chip under the field before you press Enter:
  "today", "tonight", "tomorrow", weekday names, "next week", "at 5", "5pm", "17:30", "in 2 hours",
  plus the Spanish equivalents. Tapping the chip removes it (the words stay as typed). English and Spanish only for now.
- **Shopping lists:** as you type, finished items that match appear as suggestions above the field
  ("Milk ↺"); tapping one brings it back unticked instead of making a duplicate (Bring!'s "recently used").
  Pressing Enter on an exact match of a finished item also brings it back.

### Details sheet (tap an item)
One sheet, everything visible, nothing behind a second menu:
title (editable) · **Remind me** chips: *Later today · Tomorrow 9:00 · Next week · Pick…* · Notes ·
Pictures (add / tap to view) · Subtasks (add field) · bottom row: **Delete**, **Move to list**.
Changes save as you go (UX-NOTES: no Save button).

### List menu (⋮ in the title bar)
Share… · Rename · Colour & icon · Shopping list on/off · Delete completed · Export · Undo changes by… · Delete list.

### Sharing (MS To Do pattern, our invite flow from UX-NOTES)
- Avatars in the title bar = who it's shared with. The **Share** action opens "Invite to {list}" with
  **Copy link**, **Share** (Android sheet), the 8-word code with Copy, and "Show QR code" on demand.
- A personal list that is shared becomes its own shared group, named after the list (the engine shares groups).
  The person never sees the word "group".
- **Join** lives in the ☰ menu ("Join a shared list"), with the code boxes from UX-NOTES.

### ☰ menu (drawer on phone; bottom of the sidebar on wide screens)
Your name · Join a shared list · Your devices (Link a device / Link this device) · Import a list file ·
Settings · Restore from a backup (only on a fresh install).

### Empty states
- First launch: one list "My tasks" exists, the add field is focused-ready, hint "Add your first task".
  A quiet line "Using it on another device? Link this one" (UX-PRINCIPLES).
- An empty list: "Nothing here yet" above the add field.

### Keyboard (desktop)
Ctrl+N or `/` focuses the add field · Enter adds · Esc leaves the field · Ctrl+Z undoes the last delete or tick ·
Ctrl+1…9 switch list · Ctrl+F filters the current list.

## Words (no technical terms ever)
"Shared with Ana", "Invite", "Join a shared list", "Your devices". Never: sync, group, peer, relay, key.
Sync happens on its own; "Sync now" stays only in the hidden diagnostics screen.

## Out on purpose
Priorities, tags, projects, boards, calendar views, habits, timers, AI. Every one of them adds a decision to
each task; the target is a list app anyone understands in five seconds.
