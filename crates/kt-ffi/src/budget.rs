//! Sobres: the budget calls (kt_tasks::budget over a list document). Amounts are cents; dates
//! "YYYY-MM-DD" in the person's own calendar (the app passes `today`).

use std::time::{SystemTime, UNIX_EPOCH};

use kt_tasks::budget as b;

use crate::{fail, KtError, KtNode};

#[derive(Clone, Debug, uniffi::Record)]
pub struct EnvelopeItem {
    pub id: String,
    pub name: String,
    /// Per month ("m"), per year ("y") or the goal's total ("g").
    pub amount: i64,
    pub kind: String,
    pub due: Option<String>,
    pub group: String,
    pub balance: i64,
    pub spent_month: i64,
    /// What it asks for this month (a yearly one's twelfth, a goal's remaining share).
    pub monthly_share: i64,
    /// Ahead (+) / behind (−) of an even spending pace; monthly envelopes only.
    pub pace: Option<i64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct AccountItem {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub start: i64,
    pub balance: i64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct BudgetView {
    pub name: String,
    pub currency: String,
    pub envelopes: Vec<EnvelopeItem>,
    pub accounts: Vec<AccountItem>,
    pub available: i64,
    pub monthly_need: i64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PartItem {
    pub envelope_id: String,
    /// The envelope's name (deleted ones too), for showing old entries.
    pub envelope_name: String,
    pub cents: i64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TxItem {
    pub id: String,
    pub kind: String,
    pub date: String,
    pub amount: i64,
    pub account_id: String,
    pub account_name: String,
    pub to_id: String,
    pub to_name: String,
    pub payee: String,
    pub note: String,
    pub by: String,
    pub at_ms: u64,
    pub parts: Vec<PartItem>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TxEdit {
    pub kind: String,
    pub date: String,
    pub amount: i64,
    pub account_id: String,
    pub to_id: String,
    pub payee: String,
    pub note: String,
    pub parts: Vec<PartItem>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SpendItem {
    pub envelope_id: String,
    pub envelope_name: String,
    pub cents: i64,
}

/// What the Add sheet pre-selects for a payee: the envelope and account used last time.
#[derive(Clone, Debug, uniffi::Record)]
pub struct PayeeItem {
    pub payee: String,
    pub envelope_id: Option<String>,
    pub account_id: String,
    pub times: u32,
}

/// A repeating entry: what it posts (`tx.date` = its first date; `tx.id` = the repeat's id) and
/// the next date it will post.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RepeatItem {
    pub id: String,
    /// "w" weekly, "2w" every two weeks, "m" monthly, "y" yearly.
    pub every: String,
    pub next: String,
    pub tx: TxItem,
}

type Names = std::collections::BTreeMap<String, String>;

fn item(t: b::Tx, envs: &Names, accts: &Names) -> TxItem {
    let name = |m: &Names, k: &str| m.get(k).cloned().unwrap_or_default();
    TxItem {
        account_name: name(accts, &t.account),
        to_name: name(accts, &t.to),
        parts: t.parts.iter().map(|(e, c)| PartItem { envelope_name: name(envs, e), envelope_id: e.clone(), cents: *c }).collect(),
        id: t.id,
        kind: t.kind,
        date: t.date,
        amount: t.amount,
        account_id: t.account,
        to_id: t.to,
        payee: t.payee,
        note: t.note,
        by: t.by,
        at_ms: t.at,
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn input(t: &TxEdit) -> b::TxInput {
    b::TxInput {
        kind: t.kind.clone(),
        date: t.date.clone(),
        amount: t.amount,
        account: t.account_id.clone(),
        to: t.to_id.clone(),
        payee: t.payee.clone(),
        note: t.note.clone(),
        parts: t.parts.iter().map(|p| (p.envelope_id.clone(), p.cents)).collect(),
    }
}

#[uniffi::export]
impl KtNode {
    pub fn budget(&self, list_id: String, today: String) -> Result<BudgetView, KtError> {
        let l = self.list(&list_id)?;
        let v = self.app.read(&l, |d| b::budget(d, &today)).map_err(fail)?;
        Ok(BudgetView {
            envelopes: v
                .envelopes
                .iter()
                .map(|e| EnvelopeItem {
                    monthly_share: b::monthly_share(e, &today),
                    pace: b::pace(e, &today),
                    id: e.id.clone(),
                    name: e.name.clone(),
                    amount: e.amount,
                    kind: e.kind.clone(),
                    due: e.due.clone(),
                    group: e.group.clone(),
                    balance: e.balance,
                    spent_month: e.spent_month,
                })
                .collect(),
            accounts: v.accounts.iter().map(|a| AccountItem { id: a.id.clone(), name: a.name.clone(), kind: a.kind.clone(), start: a.start, balance: a.balance }).collect(),
            name: v.name,
            currency: v.currency,
            available: v.available,
            monthly_need: v.monthly_need,
        })
    }

    pub fn budget_activity(&self, list_id: String) -> Result<Vec<TxItem>, KtError> {
        let l = self.list(&list_id)?;
        self.app
            .read(&l, |d| {
                let (envs, accts) = (b::envelope_names(d), b::account_names(d));
                b::transactions(d).into_iter().map(|t| item(t, &envs, &accts)).collect()
            })
            .map_err(fail)
    }

    /// Add (`tx_id` None) or change an entry. Returns its id.
    pub fn set_tx(&self, list_id: String, tx_id: Option<String>, tx: TxEdit) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        let by = self.app.display_name();
        let id = self.app.edit(&l, |d| b::set_tx(d, tx_id.as_deref(), &input(&tx), &by, now_ms())).map_err(fail)?;
        self.notify();
        Ok(id)
    }

    pub fn delete_tx(&self, list_id: String, tx_id: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::delete_tx(d, &tx_id)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Undo a delete: the entry as `budget_activity` showed it.
    pub fn restore_tx(&self, list_id: String, tx: TxItem) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let t = b::Tx {
            id: tx.id,
            kind: tx.kind,
            date: tx.date,
            amount: tx.amount,
            account: tx.account_id,
            to: tx.to_id,
            payee: tx.payee,
            note: tx.note,
            by: tx.by,
            at: tx.at_ms,
            parts: tx.parts.into_iter().map(|p| (p.envelope_id, p.cents)).collect(),
        };
        self.app.edit(&l, |d| b::restore_tx(d, &t)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn set_envelope(&self, list_id: String, envelope_id: Option<String>, name: String, amount: i64, kind: String, due: Option<String>, group: String) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        let id = self.app.edit(&l, |d| b::set_envelope(d, envelope_id.as_deref(), &name, amount, &kind, due.as_deref(), &group)).map_err(fail)?;
        self.notify();
        Ok(id)
    }

    pub fn delete_envelope(&self, list_id: String, envelope_id: String, undo: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::delete_envelope(d, &envelope_id, now_ms(), undo)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn move_envelope(&self, list_id: String, envelope_id: String, after_id: Option<String>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::move_envelope(d, &envelope_id, after_id.as_deref())).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn set_account(&self, list_id: String, account_id: Option<String>, name: String, kind: String, start: i64) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        let id = self.app.edit(&l, |d| b::set_account(d, account_id.as_deref(), &name, &kind, start)).map_err(fail)?;
        self.notify();
        Ok(id)
    }

    pub fn delete_account(&self, list_id: String, account_id: String, undo: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::delete_account(d, &account_id, now_ms(), undo)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn set_currency(&self, list_id: String, currency: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::set_currency(d, &currency)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// The parts of "fill every envelope": rule "add" (each one's monthly share) or "top" (up to it).
    pub fn fill_plan(&self, list_id: String, rule: String, today: String) -> Result<Vec<PartItem>, KtError> {
        let l = self.list(&list_id)?;
        self.app
            .read(&l, |d| {
                let v = b::budget(d, &today);
                b::fill_plan(&v, &rule, &today)
                    .into_iter()
                    .map(|(e, c)| PartItem { envelope_name: v.envelopes.iter().find(|x| x.id == e).map(|x| x.name.clone()).unwrap_or_default(), envelope_id: e, cents: c })
                    .collect()
            })
            .map_err(fail)
    }

    /// Spending per envelope between two dates (both included), biggest first.
    pub fn spending(&self, list_id: String, from: String, to: String) -> Result<Vec<SpendItem>, KtError> {
        let l = self.list(&list_id)?;
        self.app
            .read(&l, |d| {
                let envs = b::envelope_names(d);
                b::spending(d, &from, &to).into_iter().map(|(e, c)| SpendItem { envelope_name: envs.get(&e).cloned().unwrap_or_default(), envelope_id: e, cents: c }).collect()
            })
            .map_err(fail)
    }

    /// Payees entered before, most used first, with what they were filed under last time.
    pub fn payees(&self, list_id: String) -> Result<Vec<PayeeItem>, KtError> {
        let l = self.list(&list_id)?;
        self.app
            .read(&l, |d| {
                let mut by: std::collections::BTreeMap<String, PayeeItem> = Default::default();
                // transactions() is newest first: the first one seen is the last used.
                for t in b::transactions(d).into_iter().filter(|t| t.kind == "exp" && !t.payee.is_empty()) {
                    let key = t.payee.to_lowercase();
                    let e = by.entry(key).or_insert_with(|| PayeeItem {
                        payee: t.payee.clone(),
                        envelope_id: if t.parts.len() == 1 { Some(t.parts[0].0.clone()) } else { None },
                        account_id: t.account.clone(),
                        times: 0,
                    });
                    e.times += 1;
                }
                let mut out: Vec<PayeeItem> = by.into_values().collect();
                out.sort_by(|a, b| b.times.cmp(&a.times).then(a.payee.cmp(&b.payee)));
                out
            })
            .map_err(fail)
    }

    /// The activity as CSV (date, kind, payee, envelope, account, amount, note, by): one line per
    /// envelope part, so a split shows as several lines.
    pub fn export_csv(&self, list_id: String) -> Result<String, KtError> {
        let rows = self.budget_activity(list_id)?;
        fn q(s: &str) -> String {
            // Quote every field; a leading = + - @ would start a formula in a spreadsheet.
            let s = if s.starts_with(['=', '+', '-', '@']) && s.parse::<f64>().is_err() { format!("'{s}") } else { s.to_string() };
            format!("\"{}\"", s.replace('"', "\"\""))
        }
        let money = |c: i64| format!("{}{}.{:02}", if c < 0 { "-" } else { "" }, c.abs() / 100, c.abs() % 100);
        let mut out = String::from("date,kind,payee,envelope,account,amount,note,by\n");
        for t in rows {
            let account = if t.kind == "xfer" { format!("{} > {}", t.account_name, t.to_name) } else { t.account_name.clone() };
            let lines: Vec<(String, i64)> = match t.kind.as_str() {
                "exp" => t.parts.iter().map(|p| (p.envelope_name.clone(), -p.cents)).collect(),
                "fill" => t.parts.iter().map(|p| (p.envelope_name.clone(), p.cents)).collect(),
                _ => vec![(String::new(), t.amount)],
            };
            for (env, c) in lines {
                out.push_str(&[q(&t.date), q(&t.kind), q(&t.payee), q(&env), q(&account), q(&money(c)), q(&t.note), q(&t.by)].join(","));
                out.push('\n');
            }
        }
        Ok(out)
    }
    pub fn repeating(&self, list_id: String) -> Result<Vec<RepeatItem>, KtError> {
        let l = self.list(&list_id)?;
        self.app
            .read(&l, |d| {
                let (envs, accts) = (b::envelope_names(d), b::account_names(d));
                b::repeats(d).into_iter().map(|r| RepeatItem { id: r.id, every: r.every, next: r.next, tx: item(r.tx, &envs, &accts) }).collect()
            })
            .map_err(fail)
    }

    /// Start (`repeat_id` None) or change a repeating entry; `tx.date` is its first date. Call
    /// [`Self::post_due`] after it to post what is due.
    pub fn set_repeating(&self, list_id: String, repeat_id: Option<String>, every: String, tx: TxEdit) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        let by = self.app.display_name();
        let id = self.app.edit(&l, |d| b::set_repeat(d, repeat_id.as_deref(), &every, &input(&tx), &by)).map_err(fail)?;
        self.notify();
        Ok(id)
    }

    /// Stop a repeating entry (what it posted stays); `undo` starts it again.
    pub fn delete_repeating(&self, list_id: String, repeat_id: String, undo: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| b::delete_repeat(d, &repeat_id, now_ms(), undo)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Post what repeating entries have due up to `today`; returns the new entries' ids (empty =
    /// nothing changed, and nothing was written).
    pub fn post_due(&self, list_id: String, today: String) -> Result<Vec<String>, KtError> {
        let l = self.list(&list_id)?;
        let due = self
            .app
            .read(&l, |d| b::repeats(d).iter().any(|r| r.next.as_str() <= today.as_str()))
            .map_err(fail)?;
        if !due {
            return Ok(Vec::new());
        }
        let ids = self.app.edit(&l, |d| b::post_due(d, &today, now_ms())).map_err(fail)?;
        if !ids.is_empty() {
            self.notify();
        }
        Ok(ids)
    }
}
