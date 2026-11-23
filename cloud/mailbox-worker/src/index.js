// Cloud mailbox (DESIGN §7b select capacity, D56): a fallback slot store for groups that
// have too few helpers, on Cloudflare's free tier. Same rules as a helper:
// - one replaceable slot per (topic, writer); a put is signed by the writer's key;
// - a new slot costs proof of work, a refresh doesn't;
// - per-topic byte cap, largest slot 640 KB, slots unrefreshed for 14 days are deleted;
// - it only ever sees ciphertext.
// The Worker only routes (free plan: 10 ms CPU); each topic is one Durable Object.
//
// Wire format (all integers little-endian):
//   PUT /v1/t/<topic hex>   body: writer 32 | version u64 | nonce u64 | sig 64 | blob
//     200 "stored" | 409 "stale" + X-Version | 428 + X-Pow-Bits | 413 too large | 507 topic full | 400 bad
//   GET /v1/t/<topic hex>?after=<rev>
//     200 body: rev u64 | { writer 32 | version u64 | rev u64 | len u32 | blob }*
//   Signed message: "kt/cloud-put/v1" | topic | version u64 | sha256(blob)
//   Proof of work: sha256("kt/cloud-pow/v1" | topic | writer | version u64 | nonce u64)
//                  has at least X-Pow-Bits leading zero bits.

import { DurableObject } from "cloudflare:workers";

const MAX_SLOT = 640 * 1024;
const TOPIC_CAP = 2 * 1024 * 1024;
// Second-pass audit T2-4: 20 bits (about a million hashes, under a second on a phone; the app accepts up to 24).
const POW_BITS = 20;
// Slots per topic (T2-4/T2-11): empty slots cost no bytes, so their number needs its own cap; a list's topic has
// one slot per member device.
const MAX_SLOTS = 64;
// One answer at most this many bytes of slots (the app stops reading at 3 MB); the rest comes on the next read.
const PAGE_BYTES = 2 * 1024 * 1024;
const TTL_MS = 14 * 86_400_000;
const SWEEP_EVERY_MS = 86_400_000;

const enc = new TextEncoder();
const hexRe = /^[0-9a-f]{64}$/;

function fromHex(h) {
  const b = new Uint8Array(h.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16);
  return b;
}

function concat(...parts) {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

function u64(n) {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(n), true);
  return b;
}

async function sha256(data) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", data));
}

function leadingZeros(h) {
  let z = 0;
  for (const byte of h) {
    if (byte === 0) {
      z += 8;
      continue;
    }
    return z + Math.clz32(byte) - 24;
  }
  return z;
}

// The network an address belongs to: IPv6 by /64 (one home or phone gets a whole /64), IPv4 as it is.
export function netOf(ip) {
  if (!ip) return "?";
  if (!ip.includes(":")) return ip;
  const [head, tail] = ip.split("::");
  const a = head ? head.split(":") : [];
  const b = tail !== undefined && tail ? tail.split(":") : [];
  const full = tail === undefined ? a : [...a, ...Array(8 - a.length - b.length).fill("0"), ...b];
  return full.slice(0, 4).map((x) => x.toLowerCase().replace(/^0+(?=.)/, "")).join(":") + "::/64";
}

const MAX_BODY = 112 + MAX_SLOT;

// A day's budget per network (second-pass audit T2-4): the per-minute limits alone let one address use the whole
// account's day. Each Worker instance counts in memory and reports every REPORT requests to that network's Budget
// object for the day (one extra request per REPORT), which says whether the day is used up.
const DAY_READS = 5000;
const DAY_WRITES = 1000;
const REPORT = 100; // one stored row per 100 requests: a busy day of 1,000 devices costs ~10,000 of the 100,000 rows
const counts = new Map(); // "day|net|r" -> requests not yet reported
const spent = new Map(); // "day|net|r" -> true when that network's day is used up

async function overBudget(env, net, write) {
  if (!env.BUDGET) return false;
  const day = new Date().toISOString().slice(0, 10);
  const k = `${day}|${net}|${write ? "w" : "r"}`;
  if (spent.get(k)) return true;
  const n = (counts.get(k) || 0) + 1;
  if (n < REPORT) {
    counts.set(k, n);
    return false;
  }
  counts.set(k, 0);
  if (counts.size > 5000) counts.clear(); // a new day, or many networks: start the tally again
  if (spent.size > 5000) spent.clear();
  try {
    const stub = env.BUDGET.get(env.BUDGET.idFromName(`${day}|${net}`));
    const r = await stub.fetch(`https://budget/add?w=${write ? 1 : 0}&n=${n}`);
    const over = (await r.text()) === "over";
    if (over) spent.set(k, true);
    return over;
  } catch (_) {
    return false;
  }
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const m = url.pathname.match(/^\/v1\/t\/([0-9a-f]{64})$/);
    if (!m) return new Response(url.pathname === "/" ? "kt cloud mailbox v1" : "not found", { status: url.pathname === "/" ? 200 : 404 });
    if (req.method !== "PUT" && req.method !== "GET") return new Response("method", { status: 405 });
    // Budgets per network before any object wakes (each wake is billed): sized far above a real household (a
    // device reads its topics every few rounds), so only a loop hits them. 429 makes the app defer to its other
    // layers (helpers, relays, direct).
    const net = netOf(req.headers.get("CF-Connecting-IP"));
    const limit = req.method === "PUT" ? env.WRITES : env.READS;
    // Fails closed (round 5, S10): a deploy that lost its limit binding answers 503 instead of running unlimited.
    if (!limit) return new Response("limits missing", { status: 503, headers: { "Retry-After": "300" } });
    if (!(await limit.limit({ key: net })).success) return new Response("slow down", { status: 429, headers: { "Retry-After": "60" } });
    if (await overBudget(env, net, req.method === "PUT")) return new Response("day used up", { status: 429, headers: { "Retry-After": "3600" } });
    if (req.method === "PUT") {
      const cl = req.headers.get("Content-Length");
      if (cl === null) return new Response("length required", { status: 411 });
      if (!(Number(cl) <= MAX_BODY)) return new Response("too large", { status: 413 });
    }
    const stub = env.TOPICS.get(env.TOPICS.idFromName(m[1]));
    return stub.fetch(req);
  },
};

// One network's count for one day. Deletes itself the next day.
export class Budget extends DurableObject {
  async fetch(req) {
    const url = new URL(req.url);
    const key = url.searchParams.get("w") === "1" ? "w" : "r";
    const add = Math.min(Number(url.searchParams.get("n")) || 0, REPORT);
    const total = ((await this.ctx.storage.get(key)) || 0) + add;
    await this.ctx.storage.put(key, total);
    if ((await this.ctx.storage.getAlarm()) === null) await this.ctx.storage.setAlarm(Date.now() + 2 * 86_400_000);
    // TEST_DAY_READS: only the local test sets it (wrangler dev --var), to reach the day's end in a few hundred reads.
    const reads = Number(this.env.TEST_DAY_READS) || DAY_READS;
    return new Response(total > (key === "w" ? DAY_WRITES : reads) ? "over" : "ok");
  }

  async alarm() {
    await this.ctx.storage.deleteAll();
  }
}

export class Topic extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
  }

  // Tables are made by the first accepted write only: a read of a topic nobody wrote leaves nothing stored.
  exists() {
    return this.sql.exec(`SELECT count(*) AS n FROM sqlite_master WHERE type = 'table' AND name = 'slots'`).one().n > 0;
  }

  create() {
    this.sql.exec(`CREATE TABLE IF NOT EXISTS slots (writer BLOB PRIMARY KEY, version INTEGER, rev INTEGER, refreshed INTEGER, blob BLOB)`);
    this.sql.exec(`CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v INTEGER)`);
  }

  rev() {
    const r = this.sql.exec(`SELECT v FROM meta WHERE k = 'rev'`).toArray();
    return r.length ? r[0].v : 0;
  }

  async fetch(req) {
    const url = new URL(req.url);
    const topic = fromHex(url.pathname.slice(6));
    return req.method === "PUT" ? this.put(topic, new Uint8Array(await req.arrayBuffer())) : this.get(Number(url.searchParams.get("after") || 0));
  }

  async put(topic, body) {
    if (body.length < 112) return new Response("bad", { status: 400 });
    const writer = body.slice(0, 32);
    const dv = new DataView(body.buffer, body.byteOffset);
    const version = dv.getBigUint64(32, true);
    const nonce = dv.getBigUint64(40, true);
    const sig = body.slice(48, 112);
    const blob = body.slice(112);
    if (blob.length > MAX_SLOT) return new Response("too large", { status: 413 });
    if (blob.length === 0) return new Response("bad", { status: 400 }); // T2-4: an empty slot is never real content
    if (version > BigInt(Number.MAX_SAFE_INTEGER)) return new Response("bad", { status: 400 });

    // The writer's signature over topic, version and blob.
    const msg = concat(enc.encode("kt/cloud-put/v1"), topic, u64(version), await sha256(blob));
    let ok = false;
    try {
      const key = await crypto.subtle.importKey("raw", writer, { name: "Ed25519" }, false, ["verify"]);
      ok = await crypto.subtle.verify({ name: "Ed25519" }, key, sig, msg);
    } catch (_) {}
    if (!ok) return new Response("bad signature", { status: 400 });

    const fresh = !this.exists();
    if (fresh) {
      const pow = await sha256(concat(enc.encode("kt/cloud-pow/v1"), topic, writer, u64(version), u64(nonce)));
      if (leadingZeros(pow) < POW_BITS) return new Response("proof of work", { status: 428, headers: { "X-Pow-Bits": String(POW_BITS) } });
      this.create();
    }
    const old = this.sql.exec(`SELECT version, length(blob) AS n FROM slots WHERE writer = ?`, writer).toArray()[0];
    // T2-10: refused, with what is stored, so a device whose clock went back writes above it (not "stored": 200 was taken as stored)
    if (old && BigInt(old.version) >= version) return new Response("stale", { status: 409, headers: { "X-Version": String(old.version) } });
    if (!old) {
      const pow = await sha256(concat(enc.encode("kt/cloud-pow/v1"), topic, writer, u64(version), u64(nonce)));
      if (leadingZeros(pow) < POW_BITS) return new Response("proof of work", { status: 428, headers: { "X-Pow-Bits": String(POW_BITS) } });
    }
    const used = this.sql.exec(`SELECT coalesce(sum(length(blob)), 0) AS n FROM slots`).one().n - (old ? old.n : 0);
    if (used + blob.length > TOPIC_CAP) return new Response("topic full", { status: 507 });
    if (!old && this.sql.exec(`SELECT count(*) AS n FROM slots`).one().n >= MAX_SLOTS) return new Response("topic full", { status: 507 });

    const rev = this.rev() + 1;
    const now = Date.now();
    this.sql.exec(`INSERT OR REPLACE INTO meta (k, v) VALUES ('rev', ?)`, rev);
    this.sql.exec(`INSERT OR REPLACE INTO slots (writer, version, rev, refreshed, blob) VALUES (?, ?, ?, ?, ?)`, writer, Number(version), rev, now, blob);
    if ((await this.ctx.storage.getAlarm()) === null) await this.ctx.storage.setAlarm(now + SWEEP_EVERY_MS);
    return new Response("stored");
  }

  get(after) {
    if (!this.exists()) return new Response(u64(0), { headers: { "Content-Type": "application/octet-stream" } });
    const rows = this.sql.exec(`SELECT writer, version, rev, blob FROM slots WHERE rev > ? ORDER BY rev`, after).toArray();
    // Paged (T2-11): when the slots don't fit one answer, the answer's rev is the last slot sent, so the app's next
    // read ("after" that rev) gets the rest. Always at least one slot.
    let n = 0;
    let bytes = 0;
    while (n < rows.length && (n === 0 || bytes + rows[n].blob.byteLength <= PAGE_BYTES)) bytes += rows[n++].blob.byteLength;
    const parts = [u64(n < rows.length ? rows[n - 1].rev : this.rev())];
    for (const r of rows.slice(0, n)) {
      const blob = new Uint8Array(r.blob);
      const len = new Uint8Array(4);
      new DataView(len.buffer).setUint32(0, blob.length, true);
      parts.push(new Uint8Array(r.writer), u64(r.version), u64(r.rev), len, blob);
    }
    return new Response(concat(...parts), { headers: { "Content-Type": "application/octet-stream" } });
  }

  // Daily: drop slots nobody refreshed for 14 days; an empty topic deletes itself.
  async alarm() {
    if (!this.exists()) return void (await this.ctx.storage.deleteAll());
    this.sql.exec(`DELETE FROM slots WHERE refreshed < ?`, Date.now() - TTL_MS);
    if (this.sql.exec(`SELECT count(*) AS n FROM slots`).one().n === 0) {
      await this.ctx.storage.deleteAll();
    } else {
      await this.ctx.storage.setAlarm(Date.now() + SWEEP_EVERY_MS);
    }
  }
}
