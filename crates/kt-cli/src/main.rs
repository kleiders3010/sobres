//! kt: command-line prototype of Kleiders Tasks. Headless; one "device" per --home dir.
//!
//! Devices sync over Nostr relays (`kt sync`), or exchange messages through files:
//!
//! ```text
//! kt --home A init                      kt --home B init
//! kt --home A new-group Family          kt --home B whoami        -> <B's device key>
//! kt --home A new-list Groceries --group Family
//! kt --home A add Groceries milk
//! kt --home A share Family <B key> --out join.bin
//!                                       kt --home B join join.bin
//!                                       kt --home B add Groceries eggs
//!                                       kt --home B send b.msgs
//! kt --home A recv b.msgs
//! kt --home A tasks Groceries           -> milk, eggs
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use kt_engine::crypto::DevicePublic;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{storage, GroupId};
use kt_tasks::app::App;
use kt_tasks::TaskView;

fn usage() -> &'static str {
    "usage: kt --home DIR <command>
  init                         create this device (identity + personal group)
  whoami                       print this device's public key
  name [NEW]                   show or set your display name
  new-group NAME               create a group (a circle of people; holds several lists)
  groups                       show groups
  new-list NAME [--group G]    create a list (default: your personal group = solo list)
  lists                        show lists by group
  move-list LIST GROUP         move a list into another group
  add LIST TITLE [--under TASK]  add a task (TASK = id prefix)
  tasks LIST                   show tasks
  done LIST TASK / undone LIST TASK
  rename LIST TASK TITLE
  rm LIST TASK
  share GROUP DEVICE_KEY --out FILE  add a device to a group; write its join bundle
  join FILE                    join a group from a bundle
  send FILE                    write this device's pending messages to FILE
  recv FILE                    apply messages from FILE
  epoch LIST                   propose a document epoch (compaction)
  info LIST                    generation, members, sizes
  sync [--relays U1,U2] [--rounds N] [--every SECS]
       [--p2p] [--iroh-relay URL] [--relay-only] [--serve SECS] [--dht]
                               sync over Nostr relays (default: the baseline relay list),
                               plus peer-to-peer with member devices (--p2p / --iroh-relay);
                               --serve keeps answering peers for SECS afterwards;
                               --dht publishes/looks up whereabouts on the Mainline DHT
  invite GROUP [--days N]      create an invite (8 words; published on the next sync)
  join-code WORD... [--relays ...] [--p2p] [--dht]
                               find an invite on every enabled layer and ask to join
  relay [--port P]             run a local test Nostr relay (no --home needed)
  helper-relay [--port P] [--token T]  run a helper iroh relay (no --home needed)"
}

fn main() {
    if std::env::args().any(|a| a == "--kt-import-worker") {
        kt_engine::sandbox::worker_main();
    }
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn open(home: &Path) -> Result<App> {
    let worker = std::env::current_exe().context("locating own executable for the import sandbox")?;
    let sandbox = SandboxConfig { worker, timeout: Duration::from_secs(20) };
    App::open(&home.join("identity"), &home.join("data"), sandbox).map_err(|e| anyhow!("{e}"))
}

fn device_key(p: &DevicePublic) -> String {
    format!("{}{}", hex::encode(p.sign), hex::encode(p.kex))
}

fn parse_device_key(s: &str) -> Result<DevicePublic> {
    let b = hex::decode(s.trim()).context("device key must be hex")?;
    if b.len() != 64 {
        bail!("device key must be 64 bytes (128 hex chars)");
    }
    let mut sign = [0u8; 32];
    let mut kex = [0u8; 32];
    sign.copy_from_slice(&b[..32]);
    kex.copy_from_slice(&b[32..]);
    Ok(DevicePublic { sign, kex })
}

fn write_messages(path: &Path, msgs: &[Vec<u8>]) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    for m in msgs {
        storage::append_record(path, m).map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

fn read_messages(path: &Path) -> Result<Vec<Vec<u8>>> {
    storage::read_records(path).map_err(|e| anyhow!("{e}"))
}

fn find_task(tasks: &[TaskView], prefix: &str) -> Result<String> {
    fn walk<'a>(ts: &'a [TaskView], out: &mut Vec<&'a TaskView>) {
        for t in ts {
            out.push(t);
            walk(&t.children, out);
        }
    }
    let mut all = Vec::new();
    walk(tasks, &mut all);
    let m: Vec<&&TaskView> = all.iter().filter(|t| t.id.starts_with(prefix) || t.title == prefix).collect();
    match m.len() {
        1 => Ok(m[0].id.clone()),
        0 => bail!("no task matches '{prefix}'"),
        _ => bail!("'{prefix}' matches several tasks; use a longer id prefix"),
    }
}

fn print_tasks(ts: &[TaskView], depth: usize) {
    for t in ts {
        println!("{}[{}] {}  ({})", "  ".repeat(depth), if t.done { "x" } else { " " }, t.title, &t.id[..8]);
        print_tasks(&t.children, depth + 1);
    }
}

/// Baseline relay list shipped with the app (updated later via the operator manifest, §7b).
/// The 8 relays that passed the E4 acceptance probe.
const BASELINE_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://nostr.mom",
    "wss://offchain.pub",
    "wss://nostr.oxtr.dev",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.snort.social",
];

fn run_relay(port: u16) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let r = kt_net::testrelay::TestRelay::start(Some(format!("127.0.0.1:{port}").parse()?)).await?;
        println!("test relay listening on {}", r.url());
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    })
}

struct P2pOpts {
    enabled: bool,
    dht: bool,
    iroh_relays: Vec<String>,
    relay_only: bool,
    serve: u64,
}

fn run_helper_relay(port: u16, token: Option<String>) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let cfg = kt_net::helper_relay::HelperRelayConfig {
            http_bind: format!("127.0.0.1:{port}").parse()?,
            tokens: token.map(|t| [t].into_iter().collect()),
            ..Default::default()
        };
        let r = kt_net::helper_relay::HelperRelay::start(cfg).await.map_err(|e| anyhow!(e))?;
        println!("helper relay listening on {}", r.url().unwrap_or_default());
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    })
}

fn run_sync(app: App, home: &Path, relays: Option<String>, rounds: usize, every: u64, p2p: P2pOpts) -> Result<()> {
    use std::sync::Arc;
    let urls: Vec<String> = match relays {
        Some(r) => r.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => BASELINE_RELAYS.iter().map(|s| s.to_string()).collect(),
    };
    let me_name = app.display_name();
    let names: std::collections::HashMap<GroupId, (String, String)> = app.groups().into_iter().map(|g| (g.group, (g.name, me_name.clone()))).collect();
    let replica = app.replica();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
        let mb = Arc::new(kt_net::nostr::NostrMailbox::new(&url_refs, kt_net::nostr::NostrPolicy::default()));
        // Real relays: the self-sustaining relay list (seed = these URLs). Local test
        // relays (ws://) keep the fixed list.
        let keeper = urls.iter().all(|u| u.starts_with("wss://")).then(|| kt_net::relays::RelayDirectory::new(&url_refs, Some(home.join("data").join("relays.dir"))));
        // One state file per relay set: mailbox indices must be stable for a given file.
        let set_id = hex::encode(&kt_engine::crypto::hash("kt/relayset/v1", urls.join(",").as_bytes())[..6]);
        let state = home.join("data").join(format!("sync-{set_id}.state"));
        let syncer = kt_net::sync::Syncer::build(
            replica.clone(),
            vec![mb.clone() as Arc<dyn kt_net::mailbox::Mailbox>],
            Some(state),
            Some(home.join("data").join("slots.store")),
        );
        if let Some(dir) = &keeper {
            kt_net::relays::install_resolver(&syncer, &mb);
            let rep = kt_net::relays::maintain(&syncer, &mb, dir).await;
            if rep.sets_created + rep.sets_repaired > 0 {
                println!("  relays: {} group set(s) created, {} repaired ({} candidates known)", rep.sets_created, rep.sets_repaired, dir.candidates().len());
            }
        }
        let mut dht_handle: Option<Arc<kt_net::dht::DhtDiscovery>> = None;
        let peer = if p2p.enabled {
            let cfg = kt_net::p2p::P2pConfig { relays: p2p.iroh_relays.clone(), relay_only: p2p.relay_only, portmap: true, ..Default::default() };
            let p = kt_net::p2p::P2p::start(syncer.clone(), cfg).await.map_err(|e| anyhow!(e))?;
            if !p2p.iroh_relays.is_empty() {
                let _ = tokio::time::timeout(Duration::from_secs(10), p.endpoint().online()).await;
            }
            p.publish_whereabouts();
            if p2p.dht {
                match kt_net::dht::DhtDiscovery::with_cache(replica.clone(), home.join("data").join("dht_nodes.txt"), None) {
                    Ok(d) => {
                        let d = Arc::new(d);
                        dht_handle = Some(d.clone());
                        let t = std::time::Instant::now();
                        let boot = tokio::time::timeout(Duration::from_secs(20), d.bootstrapped()).await.unwrap_or(false);
                        let pubr = d.publish().await;
                        let look = d.lookup().await;
                        let cached = d.save_cache().await;
                        println!(
                            "  dht: bootstrapped {boot}, published {}/{}, found {} member record(s), applied {}, {cached} nodes remembered ({:.1} s)",
                            pubr.published, pubr.published + pubr.publish_failed, look.found, look.applied, t.elapsed().as_secs_f64()
                        );
                    }
                    Err(e) => println!("  dht: unavailable ({e})"),
                }
            }
            Some(p)
        } else {
            None
        };
        for i in 0..rounds {
            if i > 0 {
                tokio::time::sleep(Duration::from_secs(every)).await;
            }
            let (w, r) = syncer.sync_once().await;
            println!(
                "round {}: wrote {} slot(s) ({} unchanged, {} waiting for budget, {} failed); read {} new slot(s), {} message(s), {} rejected",
                i + 1, w.written, w.unchanged, w.deferred, w.failed, r.slots_opened, r.messages, r.rejected
            );
            if let Some(p) = &peer {
                let pr = p.sync_round().await;
                println!(
                    "  p2p: reached {}/{} member device(s), sent {} slot(s) ({} confirmed), received {} new",
                    pr.peers_reached, pr.peers_tried, pr.sent, pr.confirmed, pr.received.slots_opened
                );
            }
        }
        // Open invites: today's record on every enabled layer, plus copies for every
        // remaining day on the storing layers (so a joiner finds it while we're offline).
        let layers = kt_net::invite_net::InviteLayers { p2p: peer.clone(), dht: dht_handle.clone(), nostr: Some(mb.clone()), helpers: None, lan: None };
        let names_fn = |g: &GroupId| names.get(g).cloned().unwrap_or_default();
        let ir = kt_net::invite_net::publish(&syncer, &layers, &names_fn).await;
        if ir.records > 0 {
            let ahead = kt_net::invite_net::publish_ahead(&syncer, &layers, &names_fn).await;
            println!("  invites: {} open; today's record on {} relay set(s), {} DHT; {ahead} day copies published ahead", ir.records, ir.nostr, ir.dht);
        }
        for p in replica.lock().expect("lock").settle_joins() {
            println!("  joined '{}' (invited by {})", p.group_name, p.inviter_name);
        }
        if let (Some(p), true) = (&peer, p2p.serve > 0) {
            println!("  answering peers for {} s", p2p.serve);
            tokio::time::sleep(Duration::from_secs(p2p.serve)).await;
            p.close().await;
        }
        for (url, h) in mb.health() {
            if h != Default::default() {
                println!(
                    "  {url}: ok {} refused {} rate-limited {} unreachable {} read-back {}/{}",
                    h.published_ok, h.published_refused, h.rate_limited, h.connect_failures, h.readback_hits, h.readback_hits + h.readback_misses
                );
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    let events = replica.lock().expect("lock").take_events();
    let parked = events.iter().filter(|e| matches!(e, kt_engine::replica::Event::Parked)).count();
    if parked > 0 {
        println!("  {parked} message(s) waiting for data not received yet");
    }
    for e in events {
        match e {
            kt_engine::replica::Event::Imported { .. } | kt_engine::replica::Event::EpochAccepted { .. } | kt_engine::replica::Event::Parked => {}
            other => println!("  {other:?}"),
        }
    }
    Ok(())
}

fn run_join_code(app: App, relays: Option<String>, words: &str, p2p: P2pOpts) -> Result<()> {
    use std::sync::Arc;
    let code = kt_engine::invite::Code::parse(words).map_err(|e| anyhow!("{e}"))?;
    let urls: Vec<String> = match relays {
        Some(r) => r.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => BASELINE_RELAYS.iter().map(|s| s.to_string()).collect(),
    };
    let replica = app.replica();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
        let mb = Arc::new(kt_net::nostr::NostrMailbox::new(&url_refs, kt_net::nostr::NostrPolicy::default()));
        let syncer = kt_net::sync::Syncer::new(replica.clone(), vec![mb.clone() as Arc<dyn kt_net::mailbox::Mailbox>]);
        if urls.iter().all(|u| u.starts_with("wss://")) {
            kt_net::relays::install_resolver(&syncer, &mb);
        }
        let peer = if p2p.enabled {
            let cfg = kt_net::p2p::P2pConfig { relays: p2p.iroh_relays.clone(), relay_only: p2p.relay_only, portmap: true, ..Default::default() };
            let p = kt_net::p2p::P2p::start(syncer.clone(), cfg).await.map_err(|e| anyhow!(e))?;
            p.publish_whereabouts();
            Some(p)
        } else {
            None
        };
        let dht = if p2p.dht {
            kt_net::dht::DhtDiscovery::new(replica.clone(), None, None).ok().map(Arc::new)
        } else {
            None
        };
        let layers = kt_net::invite_net::InviteLayers { p2p: peer.clone(), dht, nostr: Some(mb.clone()), helpers: None, lan: None };
        let t = std::time::Instant::now();
        let found = kt_net::invite_net::fetch(&layers, &code, kt_engine::invite::CodeKind::Invite, &[], Duration::from_millis(0)).await;
        let Some((rec, source)) = found else {
            if kt_net::invite_net::fetch(&layers, &code, kt_engine::invite::CodeKind::Link, &[], Duration::from_millis(0)).await.is_some() {
                bail!("this is a device link code, not an invite: use it to link this device to your own account");
            }
            bail!("no invite found for these words (check them, or it may have expired or been cancelled)");
        };
        println!(
            "found an invite to '{}' from {} via {:?} in {:.1} s (valid for {} more day(s))",
            rec.group_name,
            rec.inviter_name,
            source,
            t.elapsed().as_secs_f64(),
            rec.expires_ms.saturating_sub(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)) / 86_400_000
        );
        let direct = kt_net::invite_net::join(&syncer, peer.as_ref(), &rec).await.map_err(|e| anyhow!(e))?;
        syncer.sync_once().await; // posts our join request where the group looks
        if direct {
            println!("joined directly through the inviter's device");
        } else {
            println!("asked to join; you'll be let in as soon as a member's device sees it (run `kt sync` to check)");
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a.as_str()) == Some("helper-relay") {
        let port = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(17450);
        let token = args.iter().position(|a| a == "--token").and_then(|i| args.get(i + 1)).cloned();
        return run_helper_relay(port, token);
    }
    if args.first().map(|a| a.as_str()) == Some("relay") {
        let port = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(7447);
        return run_relay(port);
    }
    let home = match args.iter().position(|a| a == "--home") {
        Some(i) if i + 1 < args.len() => {
            let h = PathBuf::from(args[i + 1].clone());
            args.drain(i..=i + 1);
            h
        }
        _ => bail!("{}", usage()),
    };
    let take_flag = |args: &mut Vec<String>, flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        if i + 1 < args.len() {
            let v = args[i + 1].clone();
            args.drain(i..=i + 1);
            Some(v)
        } else {
            None
        }
    };
    let out = take_flag(&mut args, "--out");
    let under = take_flag(&mut args, "--under");
    let group_flag = take_flag(&mut args, "--group");
    let relays_flag = take_flag(&mut args, "--relays");
    let rounds: usize = take_flag(&mut args, "--rounds").and_then(|x| x.parse().ok()).unwrap_or(1);
    let every: u64 = take_flag(&mut args, "--every").and_then(|x| x.parse().ok()).unwrap_or(45);
    let iroh_relay = take_flag(&mut args, "--iroh-relay");
    let serve: u64 = take_flag(&mut args, "--serve").and_then(|x| x.parse().ok()).unwrap_or(0);
    let take_switch = |args: &mut Vec<String>, flag: &str| -> bool {
        match args.iter().position(|a| a == flag) {
            Some(i) => {
                args.remove(i);
                true
            }
            None => false,
        }
    };
    let p2p_flag = take_switch(&mut args, "--p2p");
    let relay_only = take_switch(&mut args, "--relay-only");
    let dht = take_switch(&mut args, "--dht");
    let p2p = P2pOpts {
        enabled: p2p_flag || iroh_relay.is_some() || dht,
        dht,
        iroh_relays: iroh_relay.map(|r| r.split(',').map(|x| x.to_string()).collect()).unwrap_or_default(),
        relay_only,
        serve,
    };
    let cmd = args.first().cloned().unwrap_or_default();
    let arg = |i: usize| args.get(i).cloned().ok_or_else(|| anyhow!("{}", usage()));
    let app = open(&home)?;
    if cmd == "sync" {
        return run_sync(app, &home, relays_flag, rounds, every, p2p);
    }
    if cmd == "join-code" {
        let words = args[1..].join(" ");
        return run_join_code(app, relays_flag, &words, p2p);
    }
    match cmd.as_str() {
        "init" => println!("device ready: {}", &device_key(&app.with(|r| r.device_public()))[..16]),
        "whoami" => println!("{}", device_key(&app.with(|r| r.device_public()))),
        "name" => match args.get(1) {
            Some(n) => app.set_display_name(n).map_err(|e| anyhow!("{e}"))?,
            None => println!("{}", app.display_name()),
        },
        "new-group" => {
            let g = app.create_group(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            println!("created group '{}' ({})", g.name, g.group.short());
        }
        "groups" => {
            for g in app.groups() {
                let members = app.with(|r| r.epochs(&g.group).and_then(|e| e.head().and_then(|h| e.recipients(&h).map(|x| x.len()))).unwrap_or(0));
                println!("{}  {}{}  ({} device{})", g.group.short(), g.name, if g.personal { " (personal)" } else { "" }, members, if members == 1 { "" } else { "s" });
            }
        }
        "new-list" => {
            let group = match &group_flag {
                Some(k) => Some(app.find_group(k).map_err(|e| anyhow!("{e}"))?.group),
                None => None,
            };
            let l = app.create_list(group, &arg(1)?).map_err(|e| anyhow!("{e}"))?;
            println!("created list '{}' in '{}' ({})", l.name, l.group_name, l.doc.short());
        }
        "lists" => {
            let mut current = String::new();
            for l in app.lists() {
                if l.group_name != current {
                    println!("{}:", l.group_name);
                    current = l.group_name.clone();
                }
                println!("  {}  ({})", l.name, l.doc.short());
            }
        }
        "move-list" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            let g = app.find_group(&arg(2)?).map_err(|e| anyhow!("{e}"))?;
            let moved = app.move_list(&l, g.group).map_err(|e| anyhow!("{e}"))?;
            println!("moved '{}' to '{}'", moved.name, moved.group_name);
        }
        "add" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            let title = arg(2)?;
            let parent = match &under {
                Some(p) => Some(find_task(&app.read(&l, kt_tasks::tasks).map_err(|e| anyhow!("{e}"))?, p)?),
                None => None,
            };
            let id = app.edit(&l, |d| kt_tasks::add_task(d, parent.as_deref(), &title)).map_err(|e| anyhow!("{e}"))?;
            println!("added {} ({})", title, &id[..8]);
        }
        "tasks" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            println!("{}:", l.name);
            print_tasks(&app.read(&l, kt_tasks::tasks).map_err(|e| anyhow!("{e}"))?, 1);
        }
        "done" | "undone" | "rm" | "rename" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            let t = find_task(&app.read(&l, kt_tasks::tasks).map_err(|e| anyhow!("{e}"))?, &arg(2)?)?;
            let res = match cmd.as_str() {
                "done" => app.edit(&l, |d| kt_tasks::set_done(d, &t, true)),
                "undone" => app.edit(&l, |d| kt_tasks::set_done(d, &t, false)),
                "rm" => app.edit(&l, |d| kt_tasks::delete_task(d, &t)),
                _ => {
                    let title = arg(3)?;
                    app.edit(&l, |d| kt_tasks::set_title(d, &t, &title))
                }
            };
            res.map_err(|e| anyhow!("{e}"))?;
        }
        "share" => {
            let g = app.find_group(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            let dev = parse_device_key(&arg(2)?)?;
            let out = PathBuf::from(out.ok_or_else(|| anyhow!("share needs --out FILE"))?);
            let mut msgs = vec![g.group.0.to_vec()];
            msgs.extend(app.share_group_with_device(g.group, dev).map_err(|e| anyhow!("{e}"))?);
            write_messages(&out, &msgs)?;
            println!("wrote join bundle for group '{}' to {}", g.name, out.display());
        }
        "join" => {
            let msgs = read_messages(Path::new(&arg(1)?))?;
            let (head, rest) = msgs.split_first().ok_or_else(|| anyhow!("empty bundle"))?;
            let g = GroupId(head.as_slice().try_into().map_err(|_| anyhow!("bad bundle"))?);
            app.join_from_bundle(g, rest).map_err(|e| anyhow!("{e}"))?;
            let name = app.groups().into_iter().find(|x| x.group == g).map(|x| x.name).unwrap_or_default();
            let lists: Vec<String> = app.lists().into_iter().filter(|l| l.group == g).map(|l| l.name).collect();
            println!("joined group '{}' ({}) with lists: {}", name, g.short(), lists.join(", "));
        }
        "send" => {
            let msgs = app.with(|r| r.outbox()).map_err(|e| anyhow!("{e}"))?;
            write_messages(Path::new(&arg(1)?), &msgs)?;
            println!("wrote {} message(s)", msgs.len());
        }
        "recv" => {
            let msgs = read_messages(Path::new(&arg(1)?))?;
            let (mut ok, mut bad) = (0, 0);
            for m in &msgs {
                match app.with(|r| r.receive(m)) {
                    Ok(()) => ok += 1,
                    Err(_) => bad += 1,
                }
            }
            let events = app.with(|r| r.take_events());
            let parked = events.iter().filter(|e| matches!(e, kt_engine::replica::Event::Parked)).count();
            println!("applied {ok} message(s), rejected {bad}, waiting {parked} (other groups or not yet decodable)");
            for e in events {
                match e {
                    kt_engine::replica::Event::Imported { .. } | kt_engine::replica::Event::EpochAccepted { .. } | kt_engine::replica::Event::Parked => {}
                    other => println!("  {other:?}"),
                }
            }
        }
        "invite" => {
            let g = app.find_group(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            if g.personal {
                bail!("your personal group is for your own devices (device linking), not invites");
            }
            let days: u64 = args.iter().position(|a| a == "--days").and_then(|i| args.get(i + 1)).and_then(|d| d.parse().ok()).unwrap_or(30).clamp(1, 30);
            let (code, expires) = app.with(|r| r.create_invite(&g.group, kt_engine::invite::CodeKind::Invite, days * 86_400_000)).map_err(|e| anyhow!("{e}"))?;
            let words: Vec<&str> = code.to_words();
            println!("invite to '{}' (valid {days} day(s), until day {}):", g.name, expires / 86_400_000);
            println!("  {}", words[..4].join(" "));
            println!("  {}", words[4..].join(" "));
            println!("(published on your next `kt sync`)");
        }
        "epoch" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            app.with(|r| r.propose_doc_epoch(&l.group, &l.doc)).map_err(|e| anyhow!("{e}"))?;
            println!("proposed generation {}", app.with(|r| r.doc_generation(&l.group, &l.doc)).unwrap_or(0));
        }
        "info" => {
            let l = app.find_list(&arg(1)?).map_err(|e| anyhow!("{e}"))?;
            let size = app.read(&l, |doc| doc.export(loro::ExportMode::Snapshot).map(|s| s.len()).unwrap_or(0)).map_err(|e| anyhow!("{e}"))?;
            println!("list '{}' in group '{}' ({}), doc {}", l.name, l.group_name, l.group.short(), l.doc.short());
            println!("generation {}  snapshot {} bytes", app.with(|r| r.doc_generation(&l.group, &l.doc)).unwrap_or(0), size);
            let epoch = app.with(|r| r.epochs(&l.group).and_then(|e| e.head().map(|h| (h.short(), e.recipients(&h).map(|x| x.len()).unwrap_or(0)))));
            if let Some((h, n)) = epoch {
                println!("epoch {h}  devices {n}");
            }
        }
        _ => bail!("{}", usage()),
    }
    Ok(())
}
