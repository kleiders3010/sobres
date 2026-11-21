# Sobres: a shared envelope budget, no account

A free, open-source envelope budget for Android and Windows, made to be shared by a couple, a family or a flat.
Give every bit of money a job ("envelopes"), record what you spend in a few taps, and everyone in the budget sees the
same numbers on their own phone. No account, no subscription, no device limit, no bank logins. Our small mailbox server
only ever holds encrypted copies (for 14 days), and the app works without it.

- **Envelopes:** each with a monthly amount, a yearly one (set aside a twelfth each month) or a goal by a date
  (split over the months left). Group them under headings. A bar shows what is left; an envelope says whether you are
  on track for the month or behind.
- **Available:** what your accounts hold minus what your envelopes hold. **Fill** gives it jobs in one step: each
  envelope's monthly amount, or just a top-up to it, or any amount you type.
- **Spent… in seconds:** amount, what for (earlier payees are suggested and pick their envelope and account), the
  envelope with what's left in it, the account. Split one purchase over several envelopes; refunds; Undo.
- **Repeating:** rent, pay, bills: any entry can come back every week, two weeks, month or year and enters
  itself on its day (once, even when two phones are offline).
- **Reconcile:** tick off each entry your bank statement shows; when the cleared balance matches the statement,
  Finish marks them reconciled.
- **Got money, Move, Transfer:** income into an account (and straight into envelopes if you like), money moved
  between envelopes, transfers between accounts (paying a card is one).
- **Activity and Month:** every entry by day, searchable, with who entered it in a shared budget; spending by
  envelope per month. CSV export.
- **Shared:** invite with a link, QR code or 8 words. Two people can spend from the same envelope at the same time,
  offline, and nothing is lost or counted twice: balances are always added up from the entries, never stored.
- **No servers you depend on:** devices reach each other directly ([iroh](https://iroh.computer)), over the same
  Wi-Fi, through public Nostr relays, the Mainline DHT, other users' apps that help carry changes, and a small
  mailbox. Every layer only ever sees encrypted data (CRDTs, [Loro](https://loro.dev)).
- English and Spanish.

Not in this first version: bank connections (they need paid aggregators and your bank password), debt payoff plans.

## Download
- Android: [Google Play](https://play.google.com/store/apps/details?id=com.jurisgames.sobres) or the APK on the
  [releases page](https://github.com/kleiders3010/jurisgames/releases)
- Windows: installer on the [releases page](https://github.com/kleiders3010/jurisgames/releases)

## Build
- Rust core (`crates/`): `cargo test --workspace` (the budget is `crates/kt-tasks/src/budget.rs`)
- Android: `scripts/build-android-libs.sh`, then `cd app && ./gradlew :android:assemblePlayRelease`
- Desktop: `scripts/package-desktop.sh`
- Screens: [docs/UI-SPEC.md](docs/UI-SPEC.md) · sync design: [docs/DESIGN.md](docs/DESIGN.md)

Sobres shares its sync engine with [Tasks](https://github.com/kleiders3010/kleiders-tasks).

## Problems or ideas

In the app: Send feedback. If the app doesn't open at all, open an issue at https://github.com/kleiders3010/sobres/issues or email hello@jurisgames.com.

## Privacy
[apps.jurisgames.com/privacy](https://apps.jurisgames.com/privacy/)

## License
Apache-2.0. Icons from Google's Material Icons (Apache-2.0).
