# UX Notes (owner's direction)

*Running list of UX decisions from the owner, recorded as given. Not built yet: they wait until the current real-device testing and battery measurements are done, so the build under test doesn't change underneath them. Principles live in `UX-PRINCIPLES.md`; this file is the concrete screen-level direction.*

## General (2026-09-25)
- **Ease of use first; far fewer technical explanations.** The current prototype explains too much ("8 words, 3 letters each is enough", etc.). Say the least that lets someone act.

## Inviting someone (2026-09-25)
Current flow is "terrible". Wanted, in this order:
1. Title: **"Invite to {group}"** (e.g. "Invite to Casita").
2. **Share link**: the link itself is **not shown in full**; just a **Copy** button, and on Android a **Share** button (system share sheet).
3. **"Or send them this code"** (*wording to improve*), then the 8-word code **clearly separated by `-`**, with its own **Copy** button.
4. **QR code**: only generated/shown **when the person picks the QR option**; **centered** when shown.

## Joining with a code (2026-09-25)
- Screen shows only the **entry boxes**, at most a short **"Enter the code"** above them, and below **"or scan a QR code"** with a **small camera icon** that opens the scanner. No other explanation text.
- **One box per word**, visually separate.
- **Typing**: once a word is certain, **autofill it and jump to the next box**. Kick in at **4 letters**, not 3: 3 letters are unique, but the 4th letter also catches a typo, so the person can fix it before moving on.
  - Word list facts (1,106 words): 3-letter prefixes are unique (checked 2026-09-25). 6 words have only 3 letters and 43 exactly 4: for those, the jump happens when the word is complete. If the 4 letters typed don't match the word the first 3 point to, it's a typo: show it and don't jump.
- **Pasting**: pasting the whole code (or the link) **anywhere fills all boxes at once** and **starts joining right away**.
- **Mobile: an easy way to paste** (e.g. a visible paste action, not only long-press).
- **Confirmation**: once the group's name is known, ask **"You are joining {group}, invited by {name}"** (confirm / cancel) before joining completes.
- Don't state that 3 letters are enough; **prove it** by autofilling.

## Waiting and loading (2026-09-26)
- Joining can take **up to a minute** on a bad connection (real test: stuck on a flaky Wi-Fi, found the group **immediately** after switching to mobile data). Nobody should feel stuck.
- A nice **"Finding…" animation**; **playful, rotating loading messages**, not one static line.
- If it takes **more than ~2 seconds**, add near the loading text: **"This might take a bit, don't close the app!"** (wording to polish).
- Same approach wherever the network makes people wait (joining, linking a device, an attachment still arriving).

## Settings (2026-09-26)
- **No Save button: every change applies immediately** when a field changes (switches, choices, theme). No Cancel either; changes stand as made.

## Also noted along the way
- **"Sync now" button**: leftover test scaffolding; syncing is automatic. Remove from the main UI (maybe keep in hidden diagnostics).
- **Themes / background images**: UX first. Background images are never shared (design already says so). Theme sharing is low priority, only once custom themes exist.
- **Required-update banner** and **"attachment still on its way"** text: placeholder looks, to be designed.
