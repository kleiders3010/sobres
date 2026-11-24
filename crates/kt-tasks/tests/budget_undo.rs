//! "Undo someone's changes" on a budget (safety audit 2026-11-24 S4-1): undo_by walked only the task tree, so in a
//! budget it always found "nothing to undo". A budget keeps envelopes ("env"), accounts ("acct"), entries ("tx") and
//! its currency ("meta") in maps: what Sam added since is removed, what Sam deleted comes back, what Sam changed goes
//! back to how it was, and what Alex did meanwhile stays.
//! pinned: names, cents and ids are this test's own stand-ins; peers 1 (Alex) and 2 (Sam)

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kt_tasks::budget::{self, TxInput};
use kt_tasks::history::undo_by;
use loro::LoroDoc;

fn tx(kind: &str, amount: i64, account: &str, payee: &str, parts: Vec<(String, i64)>) -> TxInput {
    TxInput { kind: kind.into(), date: "2026-11-20".into(), amount, account: account.into(), to: String::new(), payee: payee.into(), note: String::new(), parts }
}

fn payees(doc: &LoroDoc) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = budget::transactions(doc).into_iter().map(|t| (t.payee, t.amount)).collect();
    v.sort();
    v
}

#[test]
fn undo_someones_budget_changes() {
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    doc.set_record_timestamp(true);
    let food = budget::set_envelope(&doc, None, "Food", 30000, "m", None, "").unwrap();
    let rent = budget::set_envelope(&doc, None, "Rent", 90000, "m", None, "").unwrap();
    let bank = budget::set_account(&doc, None, "Bank", "bank", 100000).unwrap();
    let shop = budget::set_tx(&doc, None, &tx("exp", 2500, &bank, "Shop", vec![(food.clone(), 2500)]), "Alex", 1).unwrap();
    let _cafe = budget::set_tx(&doc, None, &tx("exp", 800, &bank, "Cafe", vec![(food.clone(), 800)]), "Alex", 2).unwrap();
    budget::set_currency(&doc, "$").unwrap();
    doc.commit();

    std::thread::sleep(Duration::from_millis(1100));
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    std::thread::sleep(Duration::from_millis(1100));

    // Sam (peer 2): renames an envelope, deletes one, changes an entry's amount and split, deletes an entry, adds
    // one, and changes the currency.
    let sam = doc.fork();
    sam.set_peer_id(2).unwrap();
    sam.set_record_timestamp(true);
    budget::set_envelope(&sam, Some(&food), "Fun", 30000, "m", None, "").unwrap();
    budget::delete_envelope(&sam, &rent, 5, false).unwrap();
    budget::set_tx(&sam, Some(&shop), &tx("exp", 99900, &bank, "Shop", vec![(food.clone(), 99900)]), "Sam", 6).unwrap();
    let cafe_id = budget::transactions(&sam).into_iter().find(|t| t.payee == "Cafe").unwrap().id;
    budget::delete_tx(&sam, &cafe_id).unwrap();
    budget::set_tx(&sam, None, &tx("exp", 5000, &bank, "Casino", vec![(food.clone(), 5000)]), "Sam", 7).unwrap();
    budget::set_currency(&sam, "€").unwrap();
    sam.commit();
    doc.import(&sam.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
    // Alex also enters something after that time: kept.
    budget::set_tx(&doc, None, &tx("exp", 1200, &bank, "Bakery", vec![(food.clone(), 1200)]), "Alex", 8).unwrap();
    doc.commit();

    let n = undo_by(&doc, &doc.fork(), &HashSet::from([2]), since).unwrap();
    assert!(n >= 6, "rename, envelope deletion, amount change, entry deletion, addition, currency undone (got {n})");
    assert_eq!(payees(&doc), [("Bakery".to_string(), 1200), ("Cafe".to_string(), 800), ("Shop".to_string(), 2500)]);
    let shop_now = budget::transactions(&doc).into_iter().find(|t| t.payee == "Shop").unwrap();
    assert_eq!(shop_now.parts, [(food.clone(), 2500)], "the split goes back too");
    let names = budget::envelope_names(&doc);
    assert_eq!(names.get(&food).map(String::as_str), Some("Food"));
    let live: Vec<String> = budget::budget(&doc, "2026-11-24").envelopes.into_iter().map(|e| e.name).collect();
    assert!(live.contains(&"Rent".to_string()), "the deleted envelope is back: {live:?}");
    assert_eq!(budget::currency_of(&doc), "$");
}

#[test]
fn nothing_of_theirs_nothing_undone() {
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    doc.set_record_timestamp(true);
    let food = budget::set_envelope(&doc, None, "Food", 30000, "m", None, "").unwrap();
    let bank = budget::set_account(&doc, None, "Bank", "bank", 100000).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    std::thread::sleep(Duration::from_millis(1100));
    budget::set_tx(&doc, None, &tx("exp", 2500, &bank, "Shop", vec![(food, 2500)]), "Alex", 1).unwrap();
    doc.commit();
    assert_eq!(undo_by(&doc, &doc.fork(), &HashSet::from([2]), since).unwrap(), 0);
    assert_eq!(payees(&doc).len(), 1);
}
