# Sobres UI spec (2026-11-20)

Reference: Goodbudget, studied from its help center and Play screenshots
(SelfSustainment `research/sobres-competitors/goodbudget.md`). Sobres keeps the envelope method and the Tasks look
(UX-PRINCIPLES.md applies unchanged); none of Goodbudget's names, art or layout.

## The app in one screen

Opening the app shows **the last budget, its envelopes, ready to record a purchase**.

```
┌───────────────────────────────────────┐
│ ☰  Home                      (A)(U) ⋮ │  budget name; avatars = shared with
│ [Envelopes] [Activity] [Accounts] [Month]│ sections (sidebar on wide screens)
│───────────────────────────────────────│
│  Available            1,250.00  [Fill] │  red when below zero
│ FOOD                          412.50   │  heading = envelope group, its total
│  Groceries               362.50        │  balance right, bold
│  ▓▓▓▓▓▓▓▓░░░░  of 400.00               │  bar = what is left of the month's amount
│  Eating out               50.00        │
│ BILLS                                  │
│  Phone                   -12.45        │  negative: red text and red bar
│ SAVING                                 │
│  Gifts (yearly)          150.00 / 600  │  annual: saved / yearly amount
│  Trip (by Feb 2027)      250.00 / 1,000│  goal: saved / goal
│───────────────────────────────────────│
│ [ + Spent…                         ] │  always there: opens Add on "Spent"
└───────────────────────────────────────┘
```

## Add (one sheet, four kinds as chips at the top)
**Spent · Got money · Move · Transfer**. Amount first (keyboard up), then:
- *Spent*: what for (payee; earlier payees suggested; picking one pre-selects the envelope and account used last
  time), envelope chips with "left" amounts, "Split" to spread over several envelopes (must add up), account chip
  (last used), date chip "Today" (tap: Yesterday / Pick…), note. A refund = the "Refund" switch.
- *Got money*: from whom, account, date; then "Put it in envelopes now" opens Fill with that amount.
- *Move*: from envelope (or Available) to envelope.
- *Transfer*: account to account (paying a card is this).
Save closes; snackbar "Saved · Undo".

## Fill
"Available 1,200.00" at the top, then one row per envelope with its amount field. Two quick buttons fill every
field: **Monthly amount** (adds each envelope's monthly share) and **Top up** (only up to the monthly share).
"Left in Available" at the bottom turns red below zero (allowed, just shown). Save = one fill entry.

## Envelope (tap a row)
Balance big; "of 400.00 a month"; pace line for monthly ones: "On track · 90.32 ahead" or "38.00 behind";
the envelope's activity; buttons **Move money**, **Edit** (name, amount, monthly / yearly / goal + date, heading),
**Delete** (money goes back to Available; "Deleted · Undo").

## Activity
Every entry, newest first, grouped by day: payee or kind, envelope(s), account, amount (spent in default ink with
"−", money in green "+"), and who entered it in a shared budget ("Ana"). Search field. Tap = edit in the Add
sheet. Swipe = delete with Undo.

## Accounts
Each account and its balance (a card shows what is owed); total. Add / edit: name, kind (bank, cash, card),
balance today. Changing the opening balance changes Available, as the sheet explains in one line.

## Month
Spending by envelope for a month (‹ month ›): horizontal bars, biggest first, amount and % of the month; total
spent vs money received. CSV export of the activity in the ⋮ menu.

## Budget menu (⋮)
Share… · Rename · Currency symbol · Export CSV · Undo changes by… · Delete budget.

## First launch
One budget "My budget" with the account "Bank" (balance asked inline: "How much is in it today?") and three example
envelopes (Groceries, Bills, Fun) the person can rename or delete. Hint over the envelopes: "Give your money jobs:
tap Fill".

## Words
Envelope / Sobre, Available / Disponible, Fill / Llenar, Spent / Gasté, Got money / Recibí, Move / Mover,
Transfer / Transferir, Account / Cuenta. Never: sync, group, peer, relay, key, unallocated, ledger, debit.

## Out on purpose (first version)
Bank connections (paid aggregators, bank passwords), loans/debt payoff plans, scheduled entries, reconcile,
weekly periods. Each is a later item if asked.
