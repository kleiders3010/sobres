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
//     200 "stored" | 200 "stale" | 428 + X-Pow-Bits | 413 too large | 507 topic full | 400 bad
//   GET /v1/t/<topic hex>?after=<rev>
//     200 body: rev u64 | { writer 32 | version u64 | rev u64 | len u32 | blob }*
//   Signed message: "kt/cloud-put/v1" | topic | version u64 | sha256(blob)
//   Proof of work: sha256("kt/cloud-pow/v1" | topic | writer | version u64 | nonce u64)
//                  has at least X-Pow-Bits leading zero bits.

import { DurableObject } from "cloudflare:workers";

const MAX_SLOT = 640 * 1024;
const TOPIC_CAP = 2 * 1024 * 1024;
const POW_BITS = 16;
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
    if (limit && !(await limit.limit({ key: net })).success) return new Response("slow down", { status: 429, headers: { "Retry-After": "60" } });
    if (req.method === "PUT") {
      const cl = req.headers.get("Content-Length");
      if (cl === null) return new Response("length required", { status: 411 });
      if (!(Number(cl) <= MAX_BODY)) return new Response("too large", { status: 413 });
    }
    const stub = env.TOPICS.get(env.TOPICS.idFromName(m[1]));
    return stub.fetch(req);
  },
};

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
    if (old && BigInt(old.version) >= version) return new Response("stale");
    if (!old) {
      const pow = await sha256(concat(enc.encode("kt/cloud-pow/v1"), topic, writer, u64(version), u64(nonce)));
      if (leadingZeros(pow) < POW_BITS) return new Response("proof of work", { status: 428, headers: { "X-Pow-Bits": String(POW_BITS) } });
    }
    const used = this.sql.exec(`SELECT coalesce(sum(length(blob)), 0) AS n FROM slots`).one().n - (old ? old.n : 0);
    if (used + blob.length > TOPIC_CAP) return new Response("topic full", { status: 507 });

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
    const parts = [u64(this.rev())];
    for (const r of rows) {
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
