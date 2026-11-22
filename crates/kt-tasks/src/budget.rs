//! Sobres: a budget with envelopes, shared (the budget domain on top of a list document).
//!
//! Schema (root maps of the document, carried through epochs and rebases like any root map):
//! - `meta`: `name`, `currency` (display symbol or code, e.g. "$", "€"; empty = none)
//! - `env`: envelope id → map { `name`, `amount` (cents per month; per year for annual; the total
//!   for a goal), `kind` ("m" monthly, "y" annual, "g" goal), `due` ("YYYY-MM-DD", annual/goal),
//!   `group` (heading it is listed under), `order` (f64), `gone` (ms when deleted) }
//! - `acct`: account id → map { `name`, `kind` ("bank", "cash", "card"), `start` (opening
//!   balance in cents; a card's is minus what is owed), `order`, `gone` }
//! - `tx`: transaction id → map { `k` kind, `d` date "YYYY-MM-DD", `amt` cents, `acct`, `to`
//!   (account, for a transfer), `payee`, `note`, `by` (who entered it), `at` (ms), `p` (map
//!   envelope id → cents), `clr` (0 not cleared, 1 cleared = seen at the bank, 2 reconciled) }
//! - an account's last finished reconcile: `rd` (statement date) and `rbal` (its ending balance)
//! - `rep`: repeating entries (see "repeating entries" below)
//!
//! Kinds: `exp` an expense (amt leaves `acct`; each part leaves its envelope; a negative amount is a
//! refund), `inc` income (amt enters `acct` and Available), `fill` (each part moves from Available
//! into its envelope; negative parts move back; a move between envelopes is a fill whose parts add
//! up to zero), `xfer` (amt moves from `acct` to `to`; envelopes untouched).
//!
//! **Balances are never stored**: they are sums over the transactions, so two people adding
//! transactions at the same time can never make the totals disagree once they have each other's.
//! Available = (accounts) − (envelopes). Deleted envelopes and accounts keep their id (`gone`)
//! so old transactions still name them; a deleted envelope's money goes back to Available.

use std::collections::BTreeMap;

use loro::{LoroDoc, LoroMap, LoroValue, ValueOrContainer};
use rand::RngCore;

use crate::{loro, Error, Result};
pub use kt_engine::replica::Authors;

pub const KINDS_ENV: [&str; 3] = ["m", "y", "g"];
pub const KINDS_ACCT: [&str; 3] = ["bank", "cash", "card"];
pub const KINDS_TX: [&str; 4] = ["exp", "inc", "fill", "xfer"];
/// Longest name, payee or note kept (characters).
pub const MAX_TEXT: usize = 200;
/// Largest amount (cents) one entry may carry: a trillion units, far above any household.
pub const MAX_CENTS: i64 = 100_000_000_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub id: String,
    pub name: String,
    pub amount: i64,
    pub kind: String,
    pub due: Option<String>,
    pub group: String,
    pub order: f64,
    pub balance: i64,
    /// Spent this month (expenses minus refunds), for the bar and the report.
    pub spent_month: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub start: i64,
    pub order: f64,
    pub balance: i64,
    /// Opening balance plus the entries marked cleared or reconciled (what the bank should show).
    pub cleared: i64,
    /// Last finished reconcile: statement date ("" = never) and its ending balance.
    pub rec_date: String,
    pub rec_balance: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx {
    pub id: String,
    pub kind: String,
    pub date: String,
    pub amount: i64,
    pub account: String,
    pub to: String,
    pub payee: String,
    pub note: String,
    pub by: String,
    pub at: u64,
    /// (envelope id, cents), sorted by envelope id.
    pub parts: Vec<(String, i64)>,
    /// [`NOT_CLEARED`], [`CLEARED`] (seen at the bank) or [`RECONCILED`] (part of a finished statement).
    pub status: i64,
    /// The document peer (one per device) that entered it and the one that changed it last: who entered it is
    /// shown from these, never from `by` alone (round 5, S5: `by` is text the writer's app chose).
    pub by_peer: Option<u64>,
    pub edit_peer: Option<u64>,
}

pub const NOT_CLEARED: i64 = 0;
pub const CLEARED: i64 = 1;
pub const RECONCILED: i64 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Budget {
    pub name: String,
    pub currency: String,
    pub envelopes: Vec<Envelope>,
    pub accounts: Vec<Account>,
    pub available: i64,
    /// Sum of the monthly amount every envelope asks for (annual and goal ones as their monthly share).
    pub monthly_need: i64,
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn clip(s: &str) -> String {
    s.trim().chars().take(MAX_TEXT).collect()
}

fn s(m: &LoroMap, k: &str) -> String {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::String(v))) => v.to_string(),
        _ => String::new(),
    }
}

fn i(m: &LoroMap, k: &str) -> i64 {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(v))) => v,
        Some(ValueOrContainer::Value(LoroValue::Double(v))) => v as i64,
        _ => 0,
    }
}

fn f(m: &LoroMap, k: &str) -> f64 {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::Double(v))) => v,
        Some(ValueOrContainer::Value(LoroValue::I64(v))) => v as f64,
        _ => 0.0,
    }
}

/// The entries of a root map whose values are maps: (id, map).
fn entries(doc: &LoroDoc, root: &str) -> Vec<(String, LoroMap)> {
    let mut out = Vec::new();
    doc.get_map(root).for_each(|k, v| {
        if let ValueOrContainer::Container(loro::Container::Map(m)) = v {
            out.push((k.to_string(), m));
        }
    });
    out
}

fn child(doc: &LoroDoc, root: &str, id: &str) -> Option<LoroMap> {
    match doc.get_map(root).get(id) {
        Some(ValueOrContainer::Container(loro::Container::Map(m))) => Some(m),
        _ => None,
    }
}

/// A real calendar day "YYYY-MM-DD" (month 1-12, day within the month).
pub fn valid_date(d: &str) -> bool {
    if !crate::valid_day(d) {
        return false;
    }
    let (Some((y, m)), Ok(day)) = (ym(d), d[8..10].parse::<i64>()) else { return false };
    (1..=12).contains(&m) && day >= 1 && day <= days_in_month(y, m)
}

fn check_cents(c: i64) -> Result<()> {
    if c.abs() > MAX_CENTS {
        return Err(Error::Invalid("amount too large".into()));
    }
    Ok(())
}

// ---------- reading ----------

/// Text from the document as shown: control characters out, at most [`MAX_TEXT`] (another member's app may have
/// written anything, round 5, S4/S9).
fn shown(t: String) -> String {
    if t.chars().count() <= MAX_TEXT && !t.chars().any(char::is_control) {
        return t;
    }
    t.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

/// An id this app makes: 16 hex characters, or a repeat's entry `<16 hex>.<date>`.
fn our_id(id: &str) -> bool {
    let hex16 = |x: &str| x.len() == 16 && x.bytes().all(|b| b.is_ascii_hexdigit());
    match id.split_once('.') {
        None => hex16(id),
        Some((rep, date)) => hex16(rep) && valid_date(date),
    }
}

/// Whether an entry as read passes the same rules as one being written (S4: data from another member is input
/// from a stranger). Envelopes and accounts may be deleted since, so only their ids' presence is not checked.
fn readable(t: &Tx) -> bool {
    let cents_ok = |c: i64| c.checked_abs().is_some_and(|a| a <= MAX_CENTS);
    if !our_id(&t.id) || !valid_date(&t.date) || !cents_ok(t.amount) || !t.parts.iter().all(|p| cents_ok(p.1)) {
        return false;
    }
    match t.kind.as_str() {
        "exp" => t.amount != 0 && !t.account.is_empty() && !t.parts.is_empty() && t.parts.iter().map(|p| p.1).sum::<i64>() == t.amount,
        "inc" => t.amount > 0 && !t.account.is_empty() && t.parts.is_empty(),
        "fill" => t.parts.iter().any(|p| p.1 != 0),
        "xfer" => t.amount > 0 && !t.account.is_empty() && !t.to.is_empty() && t.account != t.to && t.parts.is_empty(),
        _ => false,
    }
}

fn read_tx(id: String, m: &LoroMap) -> Option<Tx> {
    read_tx_by(id, m, &View::Direct)
}

/// How the writing devices of a budget's entries are told (round 5, S5): a document rebuild re-writes every entry
/// under the rebuilding device, so after one the table kept at the rebuild names who entered and last changed each.
enum View {
    Direct,
    Kept { rebuilder: u64, table: BTreeMap<String, Kept> },
    Unknown,
}

/// One entry as it stood right before a rebuild. 0 = not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    author: u64,
    editor: u64,
    at: u64,
    /// [`values_hash`] then: equal now means nobody has changed the entry since the rebuild.
    hash: u64,
}

impl View {
    fn of(a: &Authors) -> View {
        match a {
            Authors::Direct => View::Direct,
            Authors::Unknown => View::Unknown,
            Authors::Kept(k) => match decode_kept(&k.table) {
                Some(table) => View::Kept { rebuilder: k.rebuilder, table },
                None => View::Unknown,
            },
        }
    }
}

fn decode_kept(mut b: &[u8]) -> Option<BTreeMap<String, Kept>> {
    let mut out = BTreeMap::new();
    let u64_at =|b: &mut &[u8]| -> Option<u64> {
        let (x, rest) = b.split_at_checked(8)?;
        *b = rest;
        Some(u64::from_le_bytes(x.try_into().ok()?))
    };
    while !b.is_empty() {
        let (n, rest) = b.split_at_checked(2)?;
        let n = u16::from_le_bytes(n.try_into().ok()?) as usize;
        let (id, rest) = rest.split_at_checked(n)?;
        let id = std::str::from_utf8(id).ok()?.to_string();
        b = rest;
        let k = Kept { author: u64_at(&mut b)?, editor: u64_at(&mut b)?, at: u64_at(&mut b)?, hash: u64_at(&mut b)? };
        out.insert(id, k);
    }
    Some(out)
}

/// FNV-1a over everything an edit can change (stable across builds: the table is kept on disk).
fn values_hash(t: &Tx) -> u64 {
    fn eat(h: &mut u64, b: &[u8]) {
        for x in b.iter().chain(std::iter::once(&0xffu8)) {
            *h ^= *x as u64;
            *h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for f in [&t.kind, &t.date, &t.account, &t.to, &t.payee, &t.note] {
        eat(&mut h, f.as_bytes());
    }
    eat(&mut h, &t.amount.to_le_bytes());
    eat(&mut h, &t.status.to_le_bytes());
    for (e, c) in &t.parts {
        eat(&mut h, e.as_bytes());
        eat(&mut h, &c.to_le_bytes());
    }
    h
}

/// The table [`View::Kept`] reads, from the document right before a rebuild (`prior`: how its writers were told).
pub fn kept_authors(before: &LoroDoc, prior: &Authors) -> Vec<u8> {
    let view = View::of(prior);
    let mut out = Vec::new();
    for (id, m) in entries(before, "tx") {
        let Some(t) = read_tx_by(id, &m, &view) else { continue };
        let Ok(n) = u16::try_from(t.id.len()) else { continue };
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(t.id.as_bytes());
        for x in [t.by_peer.unwrap_or(0), t.edit_peer.unwrap_or(0), t.at, values_hash(&t)] {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    out
}

fn read_tx_by(id: String, m: &LoroMap, view: &View) -> Option<Tx> {
    let kind = s(m, "k");
    if !KINDS_TX.contains(&kind.as_str()) {
        return None; // a newer version's kind: kept in the document, not counted
    }
    let mut parts = Vec::new();
    if let Some(ValueOrContainer::Container(loro::Container::Map(p))) = m.get("p") {
        p.for_each(|e, v| {
            let c = match v {
                ValueOrContainer::Value(LoroValue::I64(c)) => c,
                ValueOrContainer::Value(LoroValue::Double(c)) if c.is_finite() && c.abs() < 9e18 => c as i64,
                _ => i64::MAX, // not a whole number of cents: fails the check below
            };
            if c != 0 {
                parts.push((e.to_string(), c));
            }
        });
    }
    parts.sort();
    let by_peer = m.get_last_editor("at");
    let editors: Vec<u64> = ["d", "amt", "k", "acct", "to", "payee", "note", "p", "clr"].iter().filter_map(|k| m.get_last_editor(k)).collect();
    let edit_peer = editors.iter().copied().find(|p| Some(*p) != by_peer).or(by_peer);
    let mut t = Tx {
        id,
        kind,
        date: s(m, "d"),
        amount: match m.get("amt") {
            Some(ValueOrContainer::Value(LoroValue::I64(v))) => v,
            _ => i64::MAX, // a float or missing: not an amount this app writes
        },
        account: s(m, "acct"),
        to: s(m, "to"),
        payee: shown(s(m, "payee")),
        note: shown(s(m, "note")),
        by: shown(s(m, "by")),
        at: i(m, "at").max(0) as u64,
        parts,
        status: i(m, "clr").clamp(NOT_CLEARED, RECONCILED),
        by_peer,
        edit_peer,
    };
    let nz = |p: u64| (p != 0).then_some(p);
    match view {
        View::Direct => {}
        // Rebuilt and nothing kept: the device that wrote "at" may be the rebuilder, so nobody is named as the
        // author (the stored label shows); a change by another device still shows.
        View::Unknown => {
            t.by_peer = None;
            t.edit_peer = editors.iter().copied().find(|p| Some(*p) != by_peer);
        }
        View::Kept { rebuilder, table } => {
            let r = *rebuilder;
            // An entry the rebuilder entered after the rebuild has no row: it is the rebuilder's own.
            if let Some(k) = table.get(&t.id).filter(|k| by_peer == Some(r) && k.at == t.at) {
                t.by_peer = nz(k.author);
                let other = editors.iter().copied().find(|p| *p != r && Some(*p) != t.by_peer);
                let rebuilder_changed = values_hash(&t) != k.hash && editors.iter().all(|p| *p == r);
                t.edit_peer = other.or(if rebuilder_changed { Some(r) } else { nz(k.editor) }).or(t.by_peer);
            }
        }
    }
    Some(t)
}

pub fn transactions(doc: &LoroDoc) -> Vec<Tx> {
    transactions_by(doc, &Authors::Direct)
}

/// The entries, naming their writers as `authors` allows (from `Replica::doc_authors`).
pub fn transactions_by(doc: &LoroDoc, authors: &Authors) -> Vec<Tx> {
    let view = View::of(authors);
    let mut out: Vec<Tx> = entries(doc, "tx").into_iter().filter_map(|(id, m)| read_tx_by(id, &m, &view)).filter(readable).collect();
    // Newest first: by date, then by when entered, then id (stable on every device).
    out.sort_by(|a, b| (&b.date, b.at, &b.id).cmp(&(&a.date, a.at, &a.id)));
    out
}

/// How many entries could not be read (written by another app with values this one refuses): they count nowhere
/// and the budget screen says so.
pub fn unreadable(doc: &LoroDoc) -> usize {
    entries(doc, "tx").into_iter().filter_map(|(id, m)| read_tx(id, &m)).filter(|t| !readable(t)).count()
}

/// Monthly share of an envelope's amount on `today` ("YYYY-MM-DD"): the amount for a monthly
/// one; a twelfth for an annual one; for a goal, what is still missing split over the months
/// left until its due date (this one included), all of it once due.
pub fn monthly_share(e: &Envelope, today: &str) -> i64 {
    match e.kind.as_str() {
        "y" => (e.amount + 11) / 12,
        "g" => {
            let missing = e.amount.saturating_sub(e.balance).max(0);
            match e.due.as_deref().and_then(months_until(today)) {
                Some(n) if n > 0 => missing.saturating_add(n - 1) / n,
                _ => missing,
            }
        }
        _ => e.amount,
    }
}

fn ym(d: &str) -> Option<(i64, i64)> {
    Some((d.get(0..4)?.parse().ok()?, d.get(5..7)?.parse().ok()?))
}

/// Months from `today`'s month to `due`'s month, both included (due this month = 1).
fn months_until(today: &str) -> impl Fn(&str) -> Option<i64> + '_ {
    move |due| {
        let (ty, tm) = ym(today)?;
        let (dy, dm) = ym(due)?;
        Some((dy * 12 + dm) - (ty * 12 + tm) + 1)
    }
}

pub fn budget(doc: &LoroDoc, today: &str) -> Budget {
    let meta = doc.get_map("meta");
    let txs = transactions(doc);
    let month = today.get(0..7).unwrap_or("");
    let mut env_bal: BTreeMap<&str, i64> = BTreeMap::new();
    let mut env_spent: BTreeMap<&str, i64> = BTreeMap::new();
    let mut acct_bal: BTreeMap<&str, i64> = BTreeMap::new();
    let mut acct_clr: BTreeMap<&str, i64> = BTreeMap::new();
    // Money maths never wraps (round 5, S4): every sum saturates.
    fn add(x: &mut i64, c: i64) {
        *x = x.saturating_add(c);
    }
    for t in &txs {
        if t.status != NOT_CLEARED {
            add(acct_clr.entry(&t.account).or_default(), effect(t, &t.account));
            if t.kind == "xfer" {
                add(acct_clr.entry(&t.to).or_default(), effect(t, &t.to));
            }
        }
        match t.kind.as_str() {
            "exp" => {
                add(acct_bal.entry(&t.account).or_default(), -t.amount);
                for (e, c) in &t.parts {
                    add(env_bal.entry(e).or_default(), -c);
                    if t.date.starts_with(month) {
                        add(env_spent.entry(e).or_default(), *c);
                    }
                }
            }
            "inc" => add(acct_bal.entry(&t.account).or_default(), t.amount),
            "fill" => {
                for (e, c) in &t.parts {
                    add(env_bal.entry(e).or_default(), *c);
                }
            }
            "xfer" => {
                add(acct_bal.entry(&t.account).or_default(), -t.amount);
                add(acct_bal.entry(&t.to).or_default(), t.amount);
            }
            _ => {}
        }
    }
    let mut envelopes: Vec<Envelope> = entries(doc, "env")
        .into_iter()
        .filter(|(_, m)| i(m, "gone") == 0)
        .map(|(id, m)| {
            let kind = s(&m, "kind");
            let kind = if KINDS_ENV.contains(&kind.as_str()) { kind } else { "m".to_string() };
            let due = Some(s(&m, "due")).filter(|d| valid_date(d));
            Envelope {
                balance: env_bal.get(id.as_str()).copied().unwrap_or(0),
                spent_month: env_spent.get(id.as_str()).copied().unwrap_or(0),
                name: shown(s(&m, "name")),
                amount: i(&m, "amount").clamp(-MAX_CENTS, MAX_CENTS),
                group: shown(s(&m, "group")),
                order: f(&m, "order"),
                kind,
                due,
                id,
            }
        })
        .collect();
    envelopes.sort_by(|a, b| a.order.total_cmp(&b.order).then(a.id.cmp(&b.id)));
    let mut accounts: Vec<Account> = entries(doc, "acct")
        .into_iter()
        .filter(|(_, m)| i(m, "gone") == 0)
        .map(|(id, m)| {
            let kind = s(&m, "kind");
            let kind = if KINDS_ACCT.contains(&kind.as_str()) { kind } else { "bank".to_string() };
            let start = i(&m, "start").clamp(-MAX_CENTS, MAX_CENTS);
            Account {
                balance: start.saturating_add(acct_bal.get(id.as_str()).copied().unwrap_or(0)),
                cleared: start.saturating_add(acct_clr.get(id.as_str()).copied().unwrap_or(0)),
                rec_date: Some(s(&m, "rd")).filter(|d| valid_date(d)).unwrap_or_default(),
                rec_balance: i(&m, "rbal"),
                name: s(&m, "name"),
                order: f(&m, "order"),
                kind,
                start,
                id,
            }
        })
        .collect();
    accounts.sort_by(|a, b| a.order.total_cmp(&b.order).then(a.id.cmp(&b.id)));
    let total = |it: &mut dyn Iterator<Item = i64>| it.fold(0i64, |a, c| a.saturating_add(c));
    let available = total(&mut accounts.iter().map(|a| a.balance)).saturating_sub(total(&mut envelopes.iter().map(|e| e.balance)));
    let monthly_need = total(&mut envelopes.iter().map(|e| monthly_share(e, today)));
    Budget { name: crate::list_name(doc), currency: s(&meta, "currency"), envelopes, accounts, available, monthly_need }
}

/// Spending per envelope between two dates (both included), biggest first: (envelope id, cents).
/// Parts naming a deleted envelope count under its id too (the caller names it "Deleted").
pub fn spending(doc: &LoroDoc, from: &str, to: &str) -> Vec<(String, i64)> {
    let mut by: BTreeMap<String, i64> = BTreeMap::new();
    for t in transactions(doc).into_iter().filter(|t| t.kind == "exp" && t.date.as_str() >= from && t.date.as_str() <= to) {
        for (e, c) in t.parts {
            let x = by.entry(e).or_default();
            *x = x.saturating_add(c);
        }
    }
    let mut out: Vec<(String, i64)> = by.into_iter().filter(|(_, c)| *c != 0).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Every envelope's name, deleted ones included (old transactions still show where they went).
pub fn envelope_names(doc: &LoroDoc) -> BTreeMap<String, String> {
    entries(doc, "env").into_iter().map(|(id, m)| (id, s(&m, "name"))).collect()
}

pub fn account_names(doc: &LoroDoc) -> BTreeMap<String, String> {
    entries(doc, "acct").into_iter().map(|(id, m)| (id, s(&m, "name"))).collect()
}

// ---------- writing ----------

pub fn set_currency(doc: &LoroDoc, currency: &str) -> Result<()> {
    doc.get_map("meta").insert("currency", currency.trim().chars().take(8).collect::<String>().as_str()).map_err(loro)?;
    doc.commit();
    Ok(())
}

fn next_order(doc: &LoroDoc, root: &str) -> f64 {
    entries(doc, root).iter().map(|(_, m)| f(m, "order")).fold(0.0, f64::max) + 1.0
}

/// Create (id None) or change an envelope. Returns its id.
pub fn set_envelope(doc: &LoroDoc, id: Option<&str>, name: &str, amount: i64, kind: &str, due: Option<&str>, group: &str) -> Result<String> {
    let name = clip(name);
    if name.is_empty() {
        return Err(Error::Invalid("name".into()));
    }
    if !KINDS_ENV.contains(&kind) || amount < 0 {
        return Err(Error::Invalid("envelope".into()));
    }
    check_cents(amount)?;
    if let Some(d) = due {
        if !valid_date(d) {
            return Err(Error::Invalid("date".into()));
        }
    }
    let (id, m) = match id {
        Some(id) => (id.to_string(), child(doc, "env", id).ok_or(Error::NotFound)?),
        None => {
            let id = new_id();
            let order = next_order(doc, "env");
            let m = doc.get_map("env").insert_container(&id, LoroMap::new()).map_err(loro)?;
            m.insert("order", order).map_err(loro)?;
            (id, m)
        }
    };
    // Only what changed is written, so two people editing different fields both keep theirs.
    if s(&m, "name") != name {
        m.insert("name", name.as_str()).map_err(loro)?;
    }
    if i(&m, "amount") != amount || m.get("amount").is_none() {
        m.insert("amount", amount).map_err(loro)?;
    }
    if s(&m, "kind") != kind {
        m.insert("kind", kind).map_err(loro)?;
    }
    let due = if kind == "m" { "" } else { due.unwrap_or("") };
    if s(&m, "due") != due {
        m.insert("due", due).map_err(loro)?;
    }
    let group = clip(group);
    if s(&m, "group") != group {
        m.insert("group", group.as_str()).map_err(loro)?;
    }
    doc.commit();
    Ok(id)
}

/// Delete an envelope (its money goes back to Available); `undo` brings it back.
pub fn delete_envelope(doc: &LoroDoc, id: &str, now_ms: u64, undo: bool) -> Result<()> {
    let m = child(doc, "env", id).ok_or(Error::NotFound)?;
    m.insert("gone", if undo { 0 } else { now_ms.max(1) as i64 }).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Put an envelope right after `after` (None = first). Fractional order, like the task tree.
pub fn move_envelope(doc: &LoroDoc, id: &str, after: Option<&str>) -> Result<()> {
    reorder(doc, "env", id, after)
}

pub fn move_account(doc: &LoroDoc, id: &str, after: Option<&str>) -> Result<()> {
    reorder(doc, "acct", id, after)
}

fn reorder(doc: &LoroDoc, root: &str, id: &str, after: Option<&str>) -> Result<()> {
    let m = child(doc, root, id).ok_or(Error::NotFound)?;
    let mut live: Vec<(f64, String)> = entries(doc, root).into_iter().filter(|(k, x)| k != id && i(x, "gone") == 0).map(|(k, x)| (f(&x, "order"), k)).collect();
    live.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let pos = match after {
        None => 0,
        Some(a) => live.iter().position(|x| x.1 == a).ok_or(Error::NotFound)? + 1,
    };
    let lo = if pos == 0 { live.first().map(|x| x.0 - 2.0).unwrap_or(0.0) } else { live[pos - 1].0 };
    let hi = live.get(pos).map(|x| x.0).unwrap_or(lo + 2.0);
    m.insert("order", (lo + hi) / 2.0).map_err(loro)?;
    doc.commit();
    Ok(())
}

pub fn set_account(doc: &LoroDoc, id: Option<&str>, name: &str, kind: &str, start: i64) -> Result<String> {
    let name = clip(name);
    if name.is_empty() {
        return Err(Error::Invalid("name".into()));
    }
    if !KINDS_ACCT.contains(&kind) {
        return Err(Error::Invalid("account".into()));
    }
    check_cents(start)?;
    let (id, m) = match id {
        Some(id) => (id.to_string(), child(doc, "acct", id).ok_or(Error::NotFound)?),
        None => {
            let id = new_id();
            let order = next_order(doc, "acct");
            let m = doc.get_map("acct").insert_container(&id, LoroMap::new()).map_err(loro)?;
            m.insert("order", order).map_err(loro)?;
            (id, m)
        }
    };
    if s(&m, "name") != name {
        m.insert("name", name.as_str()).map_err(loro)?;
    }
    if s(&m, "kind") != kind {
        m.insert("kind", kind).map_err(loro)?;
    }
    if i(&m, "start") != start || m.get("start").is_none() {
        m.insert("start", start).map_err(loro)?;
    }
    doc.commit();
    Ok(id)
}

pub fn delete_account(doc: &LoroDoc, id: &str, now_ms: u64, undo: bool) -> Result<()> {
    let m = child(doc, "acct", id).ok_or(Error::NotFound)?;
    m.insert("gone", if undo { 0 } else { now_ms.max(1) as i64 }).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// What a person enters; `id` None adds a new transaction.
#[derive(Clone, Debug, Default)]
pub struct TxInput {
    pub kind: String,
    pub date: String,
    pub amount: i64,
    pub account: String,
    pub to: String,
    pub payee: String,
    pub note: String,
    pub parts: Vec<(String, i64)>,
}

fn validate(doc: &LoroDoc, t: &TxInput) -> Result<()> {
    if !KINDS_TX.contains(&t.kind.as_str()) {
        return Err(Error::Invalid("kind".into()));
    }
    if !valid_date(&t.date) {
        return Err(Error::Invalid("date".into()));
    }
    check_cents(t.amount)?;
    let accts = account_names(doc);
    let envs = envelope_names(doc);
    let acct_ok = |a: &str| accts.contains_key(a);
    match t.kind.as_str() {
        "exp" => {
            if !acct_ok(&t.account) || t.amount == 0 || t.parts.is_empty() {
                return Err(Error::Invalid("expense".into()));
            }
            if t.parts.iter().map(|p| p.1).sum::<i64>() != t.amount {
                return Err(Error::Invalid("split".into()));
            }
        }
        "inc" => {
            if !acct_ok(&t.account) || t.amount <= 0 {
                return Err(Error::Invalid("income".into()));
            }
        }
        "fill" => {
            if t.parts.iter().all(|p| p.1 == 0) {
                return Err(Error::Invalid("fill".into()));
            }
        }
        "xfer" => {
            if !acct_ok(&t.account) || !acct_ok(&t.to) || t.account == t.to || t.amount <= 0 {
                return Err(Error::Invalid("transfer".into()));
            }
        }
        _ => unreachable!(),
    }
    for (e, c) in &t.parts {
        if !envs.contains_key(e) {
            return Err(Error::Invalid("envelope".into()));
        }
        check_cents(*c)?;
    }
    Ok(())
}

/// Add (id None) or change a transaction. `by` = the name of who enters it (kept from the first entry).
pub fn set_tx(doc: &LoroDoc, id: Option<&str>, t: &TxInput, by: &str, now_ms: u64) -> Result<String> {
    validate(doc, t)?;
    let (id, m) = match id {
        Some(id) => (id.to_string(), child(doc, "tx", id).ok_or(Error::NotFound)?),
        None => {
            let id = new_id();
            let m = doc.get_map("tx").insert_container(&id, LoroMap::new()).map_err(loro)?;
            m.insert("by", clip(by).as_str()).map_err(loro)?;
            m.insert("at", now_ms as i64).map_err(loro)?;
            (id, m)
        }
    };
    // Changing the amount, kind or accounts of a cleared entry means it no longer matches the bank.
    if i(&m, "clr") != NOT_CLEARED && (s(&m, "k") != t.kind || i(&m, "amt") != t.amount || s(&m, "acct") != t.account || s(&m, "to") != t.to) {
        m.insert("clr", NOT_CLEARED).map_err(loro)?;
    }
    let text = [("k", t.kind.clone()), ("d", t.date.clone()), ("acct", t.account.clone()), ("to", t.to.clone()), ("payee", clip(&t.payee)), ("note", clip(&t.note))];
    for (k, v) in text {
        if s(&m, k) != v {
            m.insert(k, v.as_str()).map_err(loro)?;
        }
    }
    if i(&m, "amt") != t.amount || m.get("amt").is_none() {
        m.insert("amt", t.amount).map_err(loro)?;
    }
    // The parts are replaced as a whole: a split is one decision.
    let mut want: Vec<(String, i64)> = t.parts.iter().filter(|p| p.1 != 0).cloned().collect();
    want.sort();
    let have = transactions_one(&m);
    if have != want || m.get("p").is_none() {
        let p = m.insert_container("p", LoroMap::new()).map_err(loro)?;
        for (e, c) in &want {
            p.insert(e, *c).map_err(loro)?;
        }
    }
    doc.commit();
    Ok(id)
}

fn transactions_one(m: &LoroMap) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if let Some(ValueOrContainer::Container(loro::Container::Map(p))) = m.get("p") {
        p.for_each(|e, v| {
            if let ValueOrContainer::Value(LoroValue::I64(c)) = v {
                if c != 0 {
                    out.push((e.to_string(), c));
                }
            }
        });
    }
    out.sort();
    out
}

/// Delete a transaction (Undo = [`restore_tx`] with the entry as it was read).
pub fn delete_tx(doc: &LoroDoc, id: &str) -> Result<()> {
    child(doc, "tx", id).ok_or(Error::NotFound)?;
    doc.get_map("tx").delete(id).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Put a deleted transaction back with its id, who entered it and when (Undo).
pub fn restore_tx(doc: &LoroDoc, t: &Tx) -> Result<()> {
    let m = doc.get_map("tx").insert_container(&t.id, LoroMap::new()).map_err(loro)?;
    for (k, v) in [("k", &t.kind), ("d", &t.date), ("acct", &t.account), ("to", &t.to), ("payee", &t.payee), ("note", &t.note), ("by", &t.by)] {
        m.insert(k, v.as_str()).map_err(loro)?;
    }
    m.insert("amt", t.amount).map_err(loro)?;
    m.insert("at", t.at as i64).map_err(loro)?;
    if t.status != NOT_CLEARED {
        m.insert("clr", t.status).map_err(loro)?;
    }
    let p = m.insert_container("p", LoroMap::new()).map_err(loro)?;
    for (e, c) in &t.parts {
        p.insert(e, *c).map_err(loro)?;
    }
    doc.commit();
    Ok(())
}

/// The parts of a fill for every envelope by one rule (the "fill all" button):
/// `add` = each envelope's monthly share; `top` = up to its monthly share where it is below.
pub fn fill_plan(b: &Budget, rule: &str, today: &str) -> Vec<(String, i64)> {
    b.envelopes
        .iter()
        .map(|e| {
            let share = monthly_share(e, today);
            let c = match rule {
                "top" => (share - e.balance).max(0),
                _ => share,
            };
            (e.id.clone(), c)
        })
        .filter(|p| p.1 > 0)
        .collect()
}

/// How far ahead (+) or behind (−) an envelope is on `today`: its balance minus what should still
/// be left if its monthly amount were spent evenly through the month. Monthly envelopes only.
pub fn pace(e: &Envelope, today: &str) -> Option<i64> {
    if e.kind != "m" || e.amount == 0 {
        return None;
    }
    let (y, m) = ym(today)?;
    let day: i64 = today.get(8..10)?.parse().ok()?;
    let days = days_in_month(y, m);
    let left_share = e.amount * (days - day + 1) / days;
    Some(e.balance - left_share)
}

pub fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

// ---------- reconcile ----------

/// What an entry does to an account's balance (0 when it doesn't touch it).
pub fn effect(t: &Tx, account: &str) -> i64 {
    match t.kind.as_str() {
        "exp" if t.account == account => -t.amount,
        "inc" if t.account == account => t.amount,
        "xfer" if t.account == account => -t.amount,
        "xfer" if t.to == account => t.amount,
        _ => 0,
    }
}

/// Mark an entry as seen at the bank (cleared) or not. Un-marking a reconciled one is allowed:
/// it simply leaves the finished statement.
pub fn set_cleared(doc: &LoroDoc, id: &str, cleared: bool) -> Result<()> {
    let m = child(doc, "tx", id).ok_or(Error::NotFound)?;
    let have = i(&m, "clr");
    let want = if cleared { CLEARED } else { NOT_CLEARED };
    if !(cleared && have == RECONCILED) && have != want {
        m.insert("clr", want).map_err(loro)?;
        doc.commit();
    }
    Ok(())
}

/// Finish a reconcile of `account` against a statement: allowed only when the cleared balance
/// equals the statement's ending balance; every cleared entry of the account becomes reconciled.
/// Returns how many entries were marked.
pub fn finish_reconcile(doc: &LoroDoc, account: &str, date: &str, statement: i64) -> Result<usize> {
    if !valid_date(date) {
        return Err(Error::Invalid("date".into()));
    }
    check_cents(statement)?;
    let a = child(doc, "acct", account).ok_or(Error::NotFound)?;
    let txs = transactions(doc);
    let cleared = txs.iter().filter(|t| t.status != NOT_CLEARED).map(|t| effect(t, account)).fold(i(&a, "start").clamp(-MAX_CENTS, MAX_CENTS), |s, c| s.saturating_add(c));
    if cleared != statement {
        return Err(Error::Invalid("difference".into()));
    }
    let mut n = 0;
    for t in txs.iter().filter(|t| t.status == CLEARED && effect(t, account) != 0) {
        if let Some(m) = child(doc, "tx", &t.id) {
            m.insert("clr", RECONCILED).map_err(loro)?;
            n += 1;
        }
    }
    a.insert("rd", date).map_err(loro)?;
    a.insert("rbal", statement).map_err(loro)?;
    doc.commit();
    Ok(n)
}

// ---------- repeating entries ----------
//
// `rep`: repeat id → map { the fields of a transaction (`k`, `d` = first date, `amt`, `acct`, `to`,
// `payee`, `note`, `p`), `every` ("w", "2w", "m", "y"), `by`, `gone`, `done` (map date → 1: the
// dates already posted or passed over) }. Each due date becomes a transaction whose id is made from
// the repeat's id and the date, so two devices posting the same date at once write the same entry
// (one, not two), and a posted entry someone deleted stays deleted (its date is in `done`).

pub const EVERY: [&str; 4] = ["w", "2w", "m", "y"];
/// At most this many entries are posted in one go (a repeat started long ago catches up over calls).
pub const MAX_POST: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repeat {
    pub id: String,
    pub every: String,
    /// The entry as a template; its `date` is the first date.
    pub tx: Tx,
    /// The next date not posted yet.
    pub next: String,
}

fn civil_days(d: &str) -> Option<i64> {
    let (y, m) = ym(d)?;
    let day: i64 = d.get(8..10)?.parse().ok()?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe)
}

fn civil_date(z: i64) -> String {
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The `n`-th date (0 = `start`) of a repeat. A monthly one on the 31st falls on the last day of
/// shorter months and comes back to the 31st after them.
pub fn occurrence(start: &str, every: &str, n: i64) -> Option<String> {
    let (y, m) = ym(start)?;
    let day: i64 = start.get(8..10)?.parse().ok()?;
    let months = |k: i64| {
        let t = y * 12 + (m - 1) + k;
        let (ny, nm) = (t.div_euclid(12), t.rem_euclid(12) + 1);
        format!("{ny:04}-{nm:02}-{:02}", day.min(days_in_month(ny, nm)))
    };
    match every {
        "w" => Some(civil_date(civil_days(start)? + 7 * n)),
        "2w" => Some(civil_date(civil_days(start)? + 14 * n)),
        "m" => Some(months(n)),
        "y" => Some(months(12 * n)),
        _ => None,
    }
}

fn done_dates(m: &LoroMap) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if let Some(ValueOrContainer::Container(loro::Container::Map(dm))) = m.get("done") {
        dm.for_each(|k, _| {
            out.insert(k.to_string());
        });
    }
    out
}

fn template(id: &str, m: &LoroMap) -> Tx {
    Tx {
        id: id.to_string(),
        kind: s(m, "k"),
        date: s(m, "d"),
        amount: i(m, "amt"),
        account: s(m, "acct"),
        to: s(m, "to"),
        payee: s(m, "payee"),
        note: s(m, "note"),
        by: s(m, "by"),
        at: 0,
        parts: transactions_one(m),
        status: NOT_CLEARED,
        by_peer: None,
        edit_peer: None,
    }
}

/// The first date of a repeat that is not posted yet (dates before `from` are skipped over).
fn next_open(start: &str, every: &str, done: &std::collections::BTreeSet<String>) -> Option<(i64, String)> {
    let mut n = 0;
    loop {
        let d = occurrence(start, every, n)?;
        if !done.contains(&d) {
            return Some((n, d));
        }
        n += 1;
        if n > 100_000 {
            return None;
        }
    }
}

/// The repeats that still run, soonest first.
pub fn repeats(doc: &LoroDoc) -> Vec<Repeat> {
    let mut out: Vec<Repeat> = entries(doc, "rep")
        .into_iter()
        .filter(|(_, m)| i(m, "gone") == 0)
        .filter_map(|(id, m)| {
            let every = s(&m, "every");
            let tx = template(&id, &m);
            if !EVERY.contains(&every.as_str()) || !KINDS_TX.contains(&tx.kind.as_str()) || !valid_date(&tx.date) {
                return None; // a newer version's kind of repeat
            }
            let (_, next) = next_open(&tx.date, &every, &done_dates(&m))?;
            Some(Repeat { id, every, tx, next })
        })
        .collect();
    out.sort_by(|a, b| (&a.next, &a.id).cmp(&(&b.next, &b.id)));
    out
}

/// Start (id None) or change a repeat. A change applies to the dates not posted yet; entries
/// already posted stay as they are (each can be changed on its own).
pub fn set_repeat(doc: &LoroDoc, id: Option<&str>, every: &str, t: &TxInput, by: &str) -> Result<String> {
    if !EVERY.contains(&every) {
        return Err(Error::Invalid("every".into()));
    }
    validate(doc, t)?;
    let (id, m) = match id {
        Some(id) => (id.to_string(), child(doc, "rep", id).ok_or(Error::NotFound)?),
        None => {
            let id = new_id();
            let m = doc.get_map("rep").insert_container(&id, LoroMap::new()).map_err(loro)?;
            m.insert("by", clip(by).as_str()).map_err(loro)?;
            m.insert_container("done", LoroMap::new()).map_err(loro)?;
            (id, m)
        }
    };
    let text = [("every", every.to_string()), ("k", t.kind.clone()), ("d", t.date.clone()), ("acct", t.account.clone()), ("to", t.to.clone()), ("payee", clip(&t.payee)), ("note", clip(&t.note))];
    for (k, v) in text {
        if s(&m, k) != v {
            m.insert(k, v.as_str()).map_err(loro)?;
        }
    }
    if i(&m, "amt") != t.amount || m.get("amt").is_none() {
        m.insert("amt", t.amount).map_err(loro)?;
    }
    let mut want: Vec<(String, i64)> = t.parts.iter().filter(|p| p.1 != 0).cloned().collect();
    want.sort();
    if transactions_one(&m) != want || m.get("p").is_none() {
        let p = m.insert_container("p", LoroMap::new()).map_err(loro)?;
        for (e, c) in &want {
            p.insert(e, *c).map_err(loro)?;
        }
    }
    doc.commit();
    Ok(id)
}

/// Stop a repeat (entries already posted stay); `undo` starts it again.
pub fn delete_repeat(doc: &LoroDoc, id: &str, now_ms: u64, undo: bool) -> Result<()> {
    let m = child(doc, "rep", id).ok_or(Error::NotFound)?;
    m.insert("gone", if undo { 0 } else { now_ms.max(1) as i64 }).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// How far back a repeat's first date and its catch-up may reach (round 5, S2).
pub const CATCH_UP_DAYS: i64 = 366;

/// Whether a new repeat may start on `start`: at most [`CATCH_UP_DAYS`] back and five years ahead of `today`.
pub fn repeat_start_ok(start: &str, today: &str) -> bool {
    match (valid_date(start).then(|| civil_days(start)).flatten(), civil_days(today)) {
        (Some(s), Some(t)) => s >= t - CATCH_UP_DAYS && s <= t + 5 * 366,
        _ => false,
    }
}

/// The UTC day of a time in ms since 1970, "YYYY-MM-DD".
pub fn day_of_ms(ms: u64) -> String {
    civil_date(civil_days("1970-01-01").unwrap_or(0) + (ms / 86_400_000) as i64)
}

/// Every entry a repeat posted (ids), for "remove what it posted".
pub fn repeat_entries(doc: &LoroDoc, rep: &str) -> Vec<String> {
    let prefix = format!("{rep}.");
    entries(doc, "tx").into_iter().filter(|(id, m)| id.starts_with(&prefix) || s(m, "rep") == rep).map(|(id, _)| id).collect()
}

/// Stop a repeat and delete every entry it posted, in one change. Returns the entries as they were (for Undo).
pub fn delete_repeat_and_entries(doc: &LoroDoc, rep: &str, now_ms: u64) -> Result<Vec<Tx>> {
    let ids: std::collections::BTreeSet<String> = repeat_entries(doc, rep).into_iter().collect();
    let gone: Vec<Tx> = entries(doc, "tx").into_iter().filter(|(id, _)| ids.contains(id)).filter_map(|(id, m)| read_tx(id, &m)).collect();
    for id in &ids {
        doc.get_map("tx").delete(id).map_err(loro)?;
    }
    if let Some(m) = child(doc, "rep", rep) {
        m.insert("gone", now_ms.max(1) as i64).map_err(loro)?;
    }
    doc.commit();
    Ok(gone)
}

/// The id of the entry a repeat posts on `date`.
pub fn repeat_tx_id(rep: &str, date: &str) -> String {
    format!("{rep}.{date}")
}

/// Post every repeat's dates up to `today` that are not posted yet. Returns the new entries' ids.
/// A date whose entry no longer fits the budget (its envelope or account was deleted for good) is
/// passed over, not retried.
pub fn post_due(doc: &LoroDoc, today: &str, now_ms: u64) -> Result<Vec<String>> {
    if !valid_date(today) {
        return Err(Error::Invalid("date".into()));
    }
    let mut posted = Vec::new();
    // Catch-up reaches at most CATCH_UP_DAYS back (round 5, S2): a repeat whose first date is long ago (a typo in
    // the year, or another member's app) posts about a year of entries at most, never decades of them.
    let cutoff = civil_days(today).unwrap_or(0) - CATCH_UP_DAYS;
    for r in repeats(doc) {
        let Some(m) = child(doc, "rep", &r.id) else { continue };
        let done = match m.get("done") {
            Some(ValueOrContainer::Container(loro::Container::Map(dm))) => dm,
            _ => m.insert_container("done", LoroMap::new()).map_err(loro)?,
        };
        let have = done_dates(&m);
        let Some((mut n, _)) = next_open(&r.tx.date, &r.every, &have) else { continue };
        while posted.len() < MAX_POST {
            let Some(d) = occurrence(&r.tx.date, &r.every, n) else { break };
            if d.as_str() > today {
                break;
            }
            n += 1;
            if have.contains(&d) || civil_days(&d).is_none_or(|x| x < cutoff) {
                continue;
            }
            done.insert(&d, 1i64).map_err(loro)?;
            let t = TxInput {
                kind: r.tx.kind.clone(),
                date: d.clone(),
                amount: r.tx.amount,
                account: r.tx.account.clone(),
                to: r.tx.to.clone(),
                payee: r.tx.payee.clone(),
                note: r.tx.note.clone(),
                parts: r.tx.parts.clone(),
            };
            if validate(doc, &t).is_err() {
                continue;
            }
            let tid = repeat_tx_id(&r.id, &d);
            let tm = doc.get_map("tx").insert_container(&tid, LoroMap::new()).map_err(loro)?;
            tm.insert("by", r.tx.by.as_str()).map_err(loro)?;
            tm.insert("at", now_ms as i64).map_err(loro)?;
            tm.insert("rep", r.id.as_str()).map_err(loro)?;
            set_tx(doc, Some(&tid), &t, "", now_ms)?;
            posted.push(tid);
        }
    }
    doc.commit();
    Ok(posted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> LoroDoc {
        let d = LoroDoc::new();
        crate::init_list(&d, "Home").unwrap();
        d
    }

    fn exp(acct: &str, env: &str, c: i64) -> TxInput {
        TxInput { kind: "exp".into(), date: "2026-11-10".into(), amount: c, account: acct.into(), parts: vec![(env.into(), c)], ..Default::default() }
    }

    #[test]
    fn money_adds_up() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 100_000).unwrap();
        let card = set_account(&d, None, "Card", "card", -5_000).unwrap();
        let food = set_envelope(&d, None, "Food", 40_000, "m", None, "").unwrap();
        let fun = set_envelope(&d, None, "Fun", 10_000, "m", None, "").unwrap();
        let today = "2026-11-10";
        assert_eq!(budget(&d, today).available, 95_000);
        set_tx(&d, None, &TxInput { kind: "fill".into(), date: today.into(), parts: vec![(food.clone(), 40_000), (fun.clone(), 10_000)], ..Default::default() }, "Ana", 1).unwrap();
        set_tx(&d, None, &exp(&card, &food, 2_500), "Ana", 2).unwrap();
        set_tx(&d, None, &TxInput { kind: "inc".into(), date: today.into(), amount: 30_000, account: bank.clone(), ..Default::default() }, "Ana", 3).unwrap();
        set_tx(&d, None, &TxInput { kind: "xfer".into(), date: today.into(), amount: 7_500, account: bank.clone(), to: card.clone(), ..Default::default() }, "Ana", 4).unwrap();
        let b = budget(&d, today);
        let bal = |id: &str| b.envelopes.iter().find(|e| e.id == id).unwrap().balance;
        assert_eq!(bal(&food), 37_500);
        assert_eq!(b.accounts.iter().find(|a| a.id == card).unwrap().balance, 0);
        // The invariant: accounts == envelopes + Available.
        let accts: i64 = b.accounts.iter().map(|a| a.balance).sum();
        let envs: i64 = b.envelopes.iter().map(|e| e.balance).sum();
        assert_eq!(accts, envs + b.available);
        assert_eq!(b.available, 95_000 + 30_000 - 50_000);
        // Deleting an envelope gives its money back to Available.
        delete_envelope(&d, &fun, 9, false).unwrap();
        assert_eq!(budget(&d, today).available, b.available + 10_000);
    }

    #[test]
    fn two_people_at_once_both_count() {
        let a = doc();
        let bank = set_account(&a, None, "Bank", "bank", 0).unwrap();
        let food = set_envelope(&a, None, "Food", 1000, "m", None, "").unwrap();
        let b = LoroDoc::new();
        b.import(&a.export(loro::ExportMode::Snapshot).unwrap()).unwrap();
        set_tx(&a, None, &exp(&bank, &food, 300), "Ana", 1).unwrap();
        set_tx(&b, None, &exp(&bank, &food, 200), "Beto", 2).unwrap();
        set_envelope(&a, Some(&food), "Groceries", 1000, "m", None, "").unwrap();
        set_envelope(&b, Some(&food), "Food", 1500, "m", None, "").unwrap();
        a.import(&b.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
        b.import(&a.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
        let (x, y) = (budget(&a, "2026-11-10"), budget(&b, "2026-11-10"));
        assert_eq!(x, y);
        assert_eq!(x.envelopes[0].balance, -500);
        assert_eq!((x.envelopes[0].name.as_str(), x.envelopes[0].amount), ("Groceries", 1500), "both field edits kept");
    }

    #[test]
    fn reconcile_against_a_statement() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 100_000).unwrap();
        let card = set_account(&d, None, "Card", "card", 0).unwrap();
        let food = set_envelope(&d, None, "Food", 40_000, "m", None, "").unwrap();
        let a = set_tx(&d, None, &exp(&bank, &food, 2_000), "Ana", 1).unwrap();
        let b = set_tx(&d, None, &exp(&bank, &food, 3_000), "Ana", 2).unwrap();
        let x = set_tx(&d, None, &TxInput { kind: "xfer".into(), date: "2026-11-10".into(), amount: 5_000, account: bank.clone(), to: card.clone(), ..Default::default() }, "Ana", 3).unwrap();
        let acct = |id: &str| budget(&d, "2026-11-10").accounts.into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(acct(&bank).cleared, 100_000, "nothing cleared: the opening balance");
        set_cleared(&d, &a, true).unwrap();
        set_cleared(&d, &x, true).unwrap();
        assert_eq!(acct(&bank).cleared, 93_000);
        assert_eq!(acct(&card).cleared, 5_000, "a transfer clears on both accounts");
        // The statement says 93,000: finishing is refused on any other number.
        assert!(finish_reconcile(&d, &bank, "2026-11-30", 90_000).is_err());
        assert_eq!(finish_reconcile(&d, &bank, "2026-11-30", 93_000).unwrap(), 2);
        let st = |id: &str| transactions(&d).into_iter().find(|t| t.id == id).unwrap().status;
        assert_eq!((st(&a), st(&b), st(&x)), (RECONCILED, NOT_CLEARED, RECONCILED));
        assert_eq!((acct(&bank).rec_date.as_str(), acct(&bank).rec_balance), ("2026-11-30", 93_000));
        // Clearing a reconciled one again doesn't demote it; changing its amount does.
        set_cleared(&d, &a, true).unwrap();
        assert_eq!(st(&a), RECONCILED);
        set_tx(&d, Some(&a), &exp(&bank, &food, 2_000), "Ana", 9).unwrap();
        assert_eq!(st(&a), RECONCILED, "same amount: still matches the bank");
        set_tx(&d, Some(&a), &exp(&bank, &food, 2_500), "Ana", 9).unwrap();
        assert_eq!(st(&a), NOT_CLEARED);
        // Undo of a delete keeps the status.
        let t = transactions(&d).into_iter().find(|t| t.id == x).unwrap();
        delete_tx(&d, &x).unwrap();
        restore_tx(&d, &t).unwrap();
        assert_eq!(st(&x), RECONCILED);
    }

    #[test]
    fn rules_hold() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 0).unwrap();
        let food = set_envelope(&d, None, "Food", 1000, "m", None, "").unwrap();
        let mut split = exp(&bank, &food, 500);
        split.parts = vec![(food.clone(), 300)];
        assert!(set_tx(&d, None, &split, "A", 1).is_err(), "split parts must add up");
        assert!(set_tx(&d, None, &exp("nope", &food, 5), "A", 1).is_err());
        assert!(set_tx(&d, None, &exp(&bank, "nope", 5), "A", 1).is_err());
        let mut bad = exp(&bank, &food, 5);
        bad.date = "2026-13-01".into();
        assert!(set_tx(&d, None, &bad, "A", 1).is_err());
        bad.date = "2026-02-29".into();
        assert!(set_tx(&d, None, &bad, "A", 1).is_err(), "not a leap year");
        bad.date = "2028-02-29".into();
        assert!(set_tx(&d, None, &bad, "A", 1).is_ok());
        assert!(set_envelope(&d, None, " ", 1, "m", None, "").is_err());
        assert!(set_envelope(&d, None, "X", 1, "z", None, "").is_err());
    }

    #[test]
    fn undo_delete_and_epoch_rebuild_keep_everything() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 10_000).unwrap();
        let food = set_envelope(&d, None, "Food", 1000, "m", None, "Home").unwrap();
        let t = set_tx(&d, None, &exp(&bank, &food, 250), "Ana", 7).unwrap();
        let before = budget(&d, "2026-11-10");
        let was = transactions(&d).into_iter().find(|x| x.id == t).unwrap();
        delete_tx(&d, &t).unwrap();
        assert_ne!(budget(&d, "2026-11-10"), before);
        restore_tx(&d, &was).unwrap();
        assert_eq!(budget(&d, "2026-11-10"), before);
        assert_eq!(transactions(&d)[0].by, "Ana");
        let fresh = crate::rebuild(&d, 99).unwrap();
        assert_eq!(budget(&fresh, "2026-11-10"), before);
        // A rebuild re-writes everything under its own peer: entries are equal apart from who wrote them last.
        let anon = |v: Vec<Tx>| v.into_iter().map(|t| Tx { by_peer: None, edit_peer: None, ..t }).collect::<Vec<_>>();
        assert_eq!(anon(transactions(&fresh)), anon(transactions(&d)));
    }

    /// pinned: who entered and who last changed an entry survive document rebuilds, from each device's own copy
    /// (round 5, S5); not known = nobody named (the stored label shows).
    #[test]
    fn authors_survive_rebuilds() {
        use kt_engine::replica::KeptAuthors;
        let (ana, ben, cy, r1, r2) = (11u64, 22u64, 33u64, 99u64, 77u64);
        let d = doc();
        d.set_peer_id(ana).unwrap();
        let bank = set_account(&d, None, "Bank", "bank", 10_000).unwrap();
        let food = set_envelope(&d, None, "Food", 1000, "m", None, "").unwrap();
        let a = set_tx(&d, None, &exp(&bank, &food, 250), "Ana", 7).unwrap();
        d.set_peer_id(ben).unwrap();
        let b = set_tx(&d, None, &exp(&bank, &food, 100), "Ana", 8).unwrap(); // Ben's device, claiming Ana's name
        set_tx(&d, Some(&a), &exp(&bank, &food, 300), "", 9).unwrap();
        let peers = |v: Vec<Tx>, id: &str| v.into_iter().find(|t| t.id == id).map(|t| (t.by_peer, t.edit_peer)).unwrap();
        assert_eq!(peers(transactions(&d), &a), (Some(ana), Some(ben)));
        assert_eq!(peers(transactions(&d), &b), (Some(ben), Some(ben)));

        // First rebuild, by r1.
        let kept1 = Authors::Kept(KeptAuthors { rebuilder: r1, table: kept_authors(&d, &Authors::Direct) });
        let g1 = crate::rebuild(&d, r1).unwrap();
        assert_eq!(peers(transactions(&g1), &a), (Some(r1), Some(r1)), "a rebuild alone names the rebuilder");
        assert_eq!(peers(transactions_by(&g1, &kept1), &a), (Some(ana), Some(ben)));
        assert_eq!(peers(transactions_by(&g1, &kept1), &b), (Some(ben), Some(ben)));
        assert_eq!(peers(transactions_by(&g1, &Authors::Unknown), &a).0, None);
        // After it: Cy changes Ana's entry, the rebuilder enters one of its own and clears Ben's.
        g1.set_peer_id(cy).unwrap();
        set_tx(&g1, Some(&a), &TxInput { note: "lunch".into(), ..exp(&bank, &food, 300) }, "", 10).unwrap();
        g1.set_peer_id(r1).unwrap();
        let c = set_tx(&g1, None, &exp(&bank, &food, 50), "R", 11).unwrap();
        set_cleared(&g1, &b, true).unwrap();
        assert_eq!(peers(transactions_by(&g1, &kept1), &a), (Some(ana), Some(cy)));
        assert_eq!(peers(transactions_by(&g1, &kept1), &c), (Some(r1), Some(r1)));
        assert_eq!(peers(transactions_by(&g1, &kept1), &b), (Some(ben), Some(r1)), "the rebuilder's own change shows");

        // Second rebuild, by r2, on top of the first table.
        let kept2 = Authors::Kept(KeptAuthors { rebuilder: r2, table: kept_authors(&g1, &kept1) });
        let g2 = crate::rebuild(&g1, r2).unwrap();
        let v = transactions_by(&g2, &kept2);
        assert_eq!(peers(v.clone(), &a), (Some(ana), Some(cy)));
        assert_eq!(peers(v.clone(), &b), (Some(ben), Some(r1)));
        assert_eq!(peers(v, &c), (Some(r1), Some(r1)));
        // A table kept on an unknown generation names nobody for what was there.
        let lost = Authors::Kept(KeptAuthors { rebuilder: r2, table: kept_authors(&g1, &Authors::Unknown) });
        assert_eq!(peers(transactions_by(&g2, &lost), &a).0, None);
        // A broken table reads as unknown, never as the rebuilder.
        let bad = Authors::Kept(KeptAuthors { rebuilder: r2, table: vec![5, 0, 1] });
        assert_eq!(peers(transactions_by(&g2, &bad), &a).0, None);
    }

    #[test]
    fn shares_pace_and_fill_plan() {
        let d = doc();
        set_account(&d, None, "Bank", "bank", 0).unwrap();
        let m = set_envelope(&d, None, "Food", 30_000, "m", None, "").unwrap();
        let y = set_envelope(&d, None, "Gifts", 60_000, "y", Some("2026-12-20"), "").unwrap();
        let g = set_envelope(&d, None, "Trip", 100_000, "g", Some("2027-02-01"), "").unwrap();
        let b = budget(&d, "2026-11-01");
        let e = |id: &str| b.envelopes.iter().find(|e| e.id == id).unwrap().clone();
        assert_eq!(monthly_share(&e(&m), "2026-11-01"), 30_000);
        assert_eq!(monthly_share(&e(&y), "2026-11-01"), 5_000);
        assert_eq!(monthly_share(&e(&g), "2026-11-01"), 25_000, "nov, dec, jan, feb");
        assert_eq!(b.monthly_need, 60_000);
        assert_eq!(fill_plan(&b, "add", "2026-11-01").len(), 3);
        // Day 1 with nothing in it: a whole month behind.
        assert_eq!(pace(&e(&m), "2026-11-01"), Some(-30_000));
        assert_eq!(pace(&e(&y), "2026-11-01"), None);
    }

    #[test]
    fn reorder_envelopes() {
        let d = doc();
        let a = set_envelope(&d, None, "A", 1, "m", None, "").unwrap();
        let b = set_envelope(&d, None, "B", 1, "m", None, "").unwrap();
        let c = set_envelope(&d, None, "C", 1, "m", None, "").unwrap();
        move_envelope(&d, &c, None).unwrap();
        move_envelope(&d, &a, Some(&b)).unwrap();
        let names: Vec<String> = budget(&d, "2026-11-01").envelopes.into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["C", "B", "A"]);
    }

    #[test]
    fn repeat_dates() {
        let o = |s: &str, e: &str, n: i64| occurrence(s, e, n).unwrap();
        assert_eq!(o("2026-01-31", "m", 1), "2026-02-28");
        assert_eq!(o("2026-01-31", "m", 2), "2026-03-31", "back to the 31st");
        assert_eq!(o("2026-11-30", "w", 1), "2026-12-07");
        assert_eq!(o("2026-12-25", "2w", 1), "2027-01-08");
        assert_eq!(o("2028-02-29", "y", 1), "2029-02-28");
        assert_eq!(o("2026-11-05", "m", -11), "2025-12-05");
        for z in [-1000i64, 0, 20_000, 738_000] {
            assert_eq!(civil_days(&civil_date(z)), Some(z));
        }
    }

    #[test]
    fn repeats_post_once_even_on_two_devices() {
        let a = doc();
        let bank = set_account(&a, None, "Bank", "bank", 100_000).unwrap();
        let rent = set_envelope(&a, None, "Rent", 50_000, "m", None, "").unwrap();
        let mut t = exp(&bank, &rent, 50_000);
        t.date = "2026-09-01".into();
        t.payee = "Rent".into();
        let r = set_repeat(&a, None, "m", &t, "Ana").unwrap();
        assert!(set_repeat(&a, None, "daily", &t, "Ana").is_err());
        let b = LoroDoc::new();
        b.import(&a.export(loro::ExportMode::Snapshot).unwrap()).unwrap();
        // Both phones open the budget on Nov 10, offline: Sep, Oct, Nov are due on each.
        assert_eq!(post_due(&a, "2026-11-10", 1).unwrap().len(), 3);
        assert_eq!(post_due(&b, "2026-11-10", 2).unwrap().len(), 3);
        assert!(post_due(&a, "2026-11-10", 3).unwrap().is_empty(), "nothing twice");
        a.import(&b.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
        b.import(&a.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
        let rents = |d: &LoroDoc| transactions(d).into_iter().filter(|x| x.payee == "Rent").count();
        assert_eq!((rents(&a), rents(&b)), (3, 3), "the same three entries, not six");
        assert_eq!(budget(&a, "2026-11-10"), budget(&b, "2026-11-10"));
        assert_eq!(transactions(&a)[0].by, "Ana");
        // A posted entry someone deletes stays deleted; the next date still comes.
        delete_tx(&a, &repeat_tx_id(&r, "2026-10-01")).unwrap();
        assert!(post_due(&a, "2026-11-10", 4).unwrap().is_empty());
        assert_eq!(repeats(&a)[0].next, "2026-12-01");
        assert_eq!(post_due(&a, "2026-12-01", 5).unwrap(), vec![repeat_tx_id(&r, "2026-12-01")]);
        // A change applies from the next date on; posted ones keep their amount.
        t.amount = 55_000;
        t.parts = vec![(rent.clone(), 55_000)];
        set_repeat(&a, Some(&r), "m", &t, "Ana").unwrap();
        post_due(&a, "2027-01-01", 6).unwrap();
        let amounts: Vec<i64> = transactions(&a).into_iter().filter(|x| x.payee == "Rent").map(|x| x.amount).collect();
        assert_eq!(amounts, [55_000, 50_000, 50_000, 50_000]);
        // Stopped: nothing more; undo starts it again with the missed dates.
        delete_repeat(&a, &r, 7, false).unwrap();
        assert!(repeats(&a).is_empty());
        assert!(post_due(&a, "2027-03-01", 8).unwrap().is_empty());
        delete_repeat(&a, &r, 9, true).unwrap();
        assert_eq!(post_due(&a, "2027-03-01", 10).unwrap().len(), 2);
        // Survives an epoch rebuild.
        let fresh = crate::rebuild(&a, 99).unwrap();
        assert_eq!(repeats(&fresh), repeats(&a));
        assert!(post_due(&fresh, "2027-03-01", 11).unwrap().is_empty());
    }

    #[test]
    fn a_repeat_whose_envelope_is_gone_passes_over() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 0).unwrap();
        let fun = set_envelope(&d, None, "Fun", 1000, "m", None, "").unwrap();
        let mut t = exp(&bank, &fun, 100);
        t.date = "2026-11-01".into();
        set_repeat(&d, None, "w", &t, "A").unwrap();
        assert_eq!(post_due(&d, "2026-11-20", 1).unwrap().len(), 3);
        d.get_map("env").delete(&fun).unwrap();
        assert!(post_due(&d, "2026-11-30", 2).unwrap().is_empty());
        assert_eq!(repeats(&d)[0].next, "2026-12-06", "passed over, not retried");
    }

    /// Round 5, S2: a weekly repeat with a first date centuries back (a typo in the year, or another member's
    /// app) posts about a year of entries at most. pinned: catch-up reaches CATCH_UP_DAYS back.
    #[test]
    fn repeat_backlog_is_capped() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 0).unwrap();
        let food = set_envelope(&d, None, "Food", 0, "m", None, "").unwrap();
        let mut t = exp(&bank, &food, 100);
        t.date = "0206-11-21".into();
        set_repeat(&d, None, "w", &t, "Ana").unwrap();
        let mut total = 0;
        for round in 0..20 {
            let n = post_due(&d, "2026-11-21", round).unwrap().len();
            total += n;
            if n == 0 {
                break;
            }
        }
        let most = (CATCH_UP_DAYS / 7 + 1) as usize;
        assert!(total > 0 && total <= most, "{total} entries posted (at most {most})");
        assert!(transactions(&d).iter().all(|t| t.date.as_str() >= "2025-11-20"));
    }

    #[test]
    fn repeat_start_window() {
        assert!(repeat_start_ok("2026-11-21", "2026-11-21"));
        assert!(repeat_start_ok("2025-11-21", "2026-11-21"), "a year back");
        assert!(!repeat_start_ok("2025-11-19", "2026-11-21"));
        assert!(!repeat_start_ok("0206-11-21", "2026-11-21"), "a typo in the year");
        assert!(!repeat_start_ok("2036-11-21", "2026-11-21"));
        assert!(!repeat_start_ok("2026-02-30", "2026-11-21"));
    }

    /// S2: one action removes everything a repeat posted, and stops it.
    #[test]
    fn remove_everything_a_repeat_posted() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 0).unwrap();
        let rent = set_envelope(&d, None, "Rent", 0, "m", None, "").unwrap();
        let mut t = exp(&bank, &rent, 50_000);
        t.date = "2026-08-01".into();
        let rep = set_repeat(&d, None, "m", &t, "Ana").unwrap();
        set_tx(&d, None, &exp(&bank, &rent, 700), "Ana", 1).unwrap(); // one of their own: stays
        assert_eq!(post_due(&d, "2026-11-20", 1).unwrap().len(), 4);
        let gone = delete_repeat_and_entries(&d, &rep, 5).unwrap();
        assert_eq!(gone.len(), 4);
        assert_eq!(transactions(&d).len(), 1);
        assert!(repeats(&d).is_empty(), "stopped");
        assert!(post_due(&d, "2026-12-20", 6).unwrap().is_empty());
    }

    /// S4 + S6: entries another app wrote with values this one refuses count nowhere and are counted as unreadable;
    /// sums never wrap. pinned: data from another member is checked on read.
    #[test]
    fn foreign_entries_are_checked_on_read() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 100_000).unwrap();
        let food = set_envelope(&d, None, "Food", 0, "m", None, "").unwrap();
        let before = budget(&d, "2026-11-21");
        let raw = |id: &str, kind: &str, date: &str, amt: i64, parts: &[(&str, i64)]| {
            let m = d.get_map("tx").insert_container(id, LoroMap::new()).unwrap();
            m.insert("k", kind).unwrap();
            m.insert("d", date).unwrap();
            m.insert("amt", amt).unwrap();
            m.insert("acct", bank.as_str()).unwrap();
            let p = m.insert_container("p", LoroMap::new()).unwrap();
            for (e, c) in parts {
                p.insert(e, *c).unwrap();
            }
            d.commit();
        };
        raw("aaaaaaaaaaaaaaa1", "inc", "2026-11-20", i64::MAX, &[]);
        raw("aaaaaaaaaaaaaaa2", "inc", "", i64::MAX, &[]);
        raw("aaaaaaaaaaaaaaa3", "exp", "2026-11-20", 0, &[(food.as_str(), -5_000_000)]);
        raw("d-2026-11-20", "inc", "2026-11-20", 100, &[]); // an id the Activity list uses for a day header
        raw("aaaaaaaaaaaaaaa4", "exp", "2026-11-20", 100, &[(food.as_str(), 200)]); // parts don't add up
        let after = budget(&d, "2026-11-21");
        assert_eq!(after.available, before.available);
        assert_eq!(after.accounts, before.accounts);
        assert_eq!(unreadable(&d), 5);
        assert!(transactions(&d).is_empty());
        // Honest entries still count, and a huge but allowed amount saturates instead of wrapping.
        set_tx(&d, None, &TxInput { kind: "inc".into(), date: "2026-11-20".into(), amount: MAX_CENTS, account: bank.clone(), ..Default::default() }, "Ana", 1).unwrap();
        assert_eq!(budget(&d, "2026-11-21").accounts[0].balance, 100_000 + MAX_CENTS);
    }

    /// S9: text from another member is shown without control characters and clipped.
    #[test]
    fn foreign_text_is_cleaned() {
        let d = doc();
        let bank = set_account(&d, None, "Bank", "bank", 0).unwrap();
        let id = set_tx(&d, None, &TxInput { kind: "inc".into(), date: "2026-11-20".into(), amount: 5, account: bank, ..Default::default() }, "Ana", 1).unwrap();
        let m = child(&d, "tx", &id).unwrap();
        m.insert("payee", "\t=evil\r\n").unwrap();
        m.insert("note", "x".repeat(5000).as_str()).unwrap();
        d.commit();
        let t = &transactions(&d)[0];
        assert_eq!(t.payee, "=evil");
        assert_eq!(t.note.chars().count(), MAX_TEXT);
    }
}
