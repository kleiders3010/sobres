# UX Principles

*Status: draft v2, 2026-09-24 (updated with research decisions). Decisions about how the app should feel. Screens and features come later; every rule here should be validated by testing with real people.*

## Decided

1. **Design for the least effort, not the most skill.** The average user has below-average problem-solving patience for a task app, and even skilled users use it distracted, one-handed, in a hurry. If something needs figuring out, it's a design bug, not a user error.
2. **The default experience is the cleanest and simplest to understand.** Nothing optional is visible until someone looks for it.
3. **Choices are something people go looking for.** Options exist (sometimes many), but live one level deeper. They never clutter or complicate the default path.
4. **Default look: like most other task apps. Simple and professional.** Familiar beats novel: people should already know how to use it from other apps.
5. **Alternative looks are equally well designed**, made for other kinds of people, not as afterthoughts. Examples to explore: compact / dense for power users, larger and calmer for simplicity, playful, high contrast.
6. **Appearance is personal, per device.** A look can be shared with a group, where members find it if they go looking. Nobody's choice changes anyone else's app.
7. **Nothing may require setup from the user, and nothing may break with scale.** Features that need the user to install, configure or sign up for something are out, unless they're optional extras.
8. **Security never costs everyone.** Protections come from informative screens that already exist, distinct formats, and easy undo, never from extra steps, shorter lifetimes or restrictions for all users.

## Derived rules (to validate through testing)

These follow from the decisions above and from established usability research (NN/g heuristics). They are not yet tested:

- **No technical words, ever.** Users never see "sync", "relay", "peer", "group key", "P2P", "CRDT", "encryption", etc. in normal use. They see "shared with Alex", "Saved", "Sent", "On Alex's phone".
- **Undo instead of confirmation dialogs.** Deleting a task happens immediately, with an Undo option. People don't read "Are you sure?" dialogs anyway.
- **Error messages say what to do**, not what went wrong.
- **Nothing requires instructions.** If a feature needs a tutorial, the design is wrong.
- **State is always visible without being noisy, and only claims what's known:** whether a list is shared, and **Saved → Sent → On Alex's phone** for changes. Never "up to date", which a P2P app can't know. Per-member freshness ("Alex's phone: 2 h ago") where useful.
- **Be honest about delays:** no "within 15 minutes" promises; say that updates can take a few hours when a phone is idle.

## Decided flows

- **"My other device" and "share with people" never appear side by side.**
  - **First launch asks nothing.** The app opens ready to use. Starting fresh and linking later loses nothing: linking merges this device's lists into the user's existing ones.
    - Opened from an invite link → it goes straight to "Join *Groceries*, shared by Uri".
    - Freshly installed from an invite link → the app picks up the invite on first launch (§5 of DESIGN.md) and shows the same join screen.
    - The empty state offers a quiet "Using it on another device? Link this one".
  - Sharing lives on the list ("Share this list"), and devices live under the user's name ("Your devices → Add a device").
  - Scanning or opening any code states what will happen: "Join *Groceries*, shared by Uri" vs "Link this phone to your lists".
  - **Linking works everywhere, and its single confirmation names what you're joining:** "This will join Mark's account: Pixel 7 · Mark's PC · 12 lists (Work, Groceries…) · your 3 lists will be combined". A link code opened from the invite path is refused with a plain explanation.
  - **"New device linked · Undo"** appears on every device for a while afterwards.
- **Invites:** one 8-word code, shared as words, link or QR. Typing needs only the first 3 letters of each word (autocomplete). The sent message shows its expiry ("valid until 24 Oct"). It's usable by any number of people, and can be cancelled any time.
- **Permissions are always explained first**, on our own screen and in plain words, and asked for only at the moment they become relevant. Examples: exact alarms when a timed reminder is first set or received; local network when a group member is on the same Wi-Fi.

## How we validate

Playtest-style: give ~5 people a task ("add milk to the shared list", "invite me to this list"), watch without helping, fix where they hesitate, repeat.
