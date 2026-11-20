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
//!   envelope id → cents) }
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
}

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

pub fn transactions(doc: &LoroDoc) -> Vec<Tx> {
    let mut out: Vec<Tx> = entries(doc, "tx")
        .into_iter()
        .filter_map(|(id, m)| {
            let kind = s(&m, "k");
            if !KINDS_TX.contains(&kind.as_str()) {
                return None; // a newer version's kind: kept in the document, not counted
            }
            let mut parts = Vec::new();
            if let Some(ValueOrContainer::Container(loro::Container::Map(p))) = m.get("p") {
                p.for_each(|e, v| {
                    let c = match v {
                        ValueOrContainer::Value(LoroValue::I64(c)) => c,
                        ValueOrContainer::Value(LoroValue::Double(c)) => c as i64,
                        _ => 0,
                    };
                    if c != 0 {
                        parts.push((e.to_string(), c));
                    }
                });
            }
            parts.sort();
            Some(Tx {
                id,
                kind,
                date: s(&m, "d"),
                amount: i(&m, "amt"),
                account: s(&m, "acct"),
                to: s(&m, "to"),
                payee: s(&m, "payee"),
                note: s(&m, "note"),
                by: s(&m, "by"),
                at: i(&m, "at").max(0) as u64,
                parts,
            })
        })
        .collect();
    // Newest first: by date, then by when entered, then id (stable on every device).
    out.sort_by(|a, b| (&b.date, b.at, &b.id).cmp(&(&a.date, a.at, &a.id)));
    out
}

/// Monthly share of an envelope's amount on `today` ("YYYY-MM-DD"): the amount for a monthly
/// one; a twelfth for an annual one; for a goal, what is still missing split over the months
/// left until its due date (this one included), all of it once due.
pub fn monthly_share(e: &Envelope, today: &str) -> i64 {
    match e.kind.as_str() {
        "y" => (e.amount + 11) / 12,
        "g" => {
            let missing = (e.amount - e.balance).max(0);
            match e.due.as_deref().and_then(months_until(today)) {
                Some(n) if n > 0 => (missing + n - 1) / n,
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
    for t in &txs {
        match t.kind.as_str() {
            "exp" => {
                *acct_bal.entry(&t.account).or_default() -= t.amount;
                for (e, c) in &t.parts {
                    *env_bal.entry(e).or_default() -= c;
                    if t.date.starts_with(month) {
                        *env_spent.entry(e).or_default() += c;
                    }
                }
            }
            "inc" => *acct_bal.entry(&t.account).or_default() += t.amount,
            "fill" => {
                for (e, c) in &t.parts {
                    *env_bal.entry(e).or_default() += c;
                }
            }
            "xfer" => {
                *acct_bal.entry(&t.account).or_default() -= t.amount;
                *acct_bal.entry(&t.to).or_default() += t.amount;
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
                name: s(&m, "name"),
                amount: i(&m, "amount"),
                group: s(&m, "group"),
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
            let start = i(&m, "start");
            Account { balance: start + acct_bal.get(id.as_str()).copied().unwrap_or(0), name: s(&m, "name"), order: f(&m, "order"), kind, start, id }
        })
        .collect();
    accounts.sort_by(|a, b| a.order.total_cmp(&b.order).then(a.id.cmp(&b.id)));
    let available = accounts.iter().map(|a| a.balance).sum::<i64>() - envelopes.iter().map(|e| e.balance).sum::<i64>();
    let monthly_need = envelopes.iter().map(|e| monthly_share(e, today)).sum();
    Budget { name: crate::list_name(doc), currency: s(&meta, "currency"), envelopes, accounts, available, monthly_need }
}

/// Spending per envelope between two dates (both included), biggest first: (envelope id, cents).
/// Parts naming a deleted envelope count under its id too (the caller names it "Deleted").
pub fn spending(doc: &LoroDoc, from: &str, to: &str) -> Vec<(String, i64)> {
    let mut by: BTreeMap<String, i64> = BTreeMap::new();
    for t in transactions(doc).into_iter().filter(|t| t.kind == "exp" && t.date.as_str() >= from && t.date.as_str() <= to) {
        for (e, c) in t.parts {
            *by.entry(e).or_default() += c;
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
        assert_eq!(transactions(&fresh), transactions(&d));
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
}
