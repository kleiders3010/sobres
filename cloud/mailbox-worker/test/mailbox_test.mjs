// The mailbox against a local `wrangler dev` (started here, stopped at the end): a signed put is stored and read
// back, bad and oversized puts are refused before the object, a read of an unknown topic answers empty, and the
// per-network read budget turns a loop into 429s (safety audit round 4, T4).
// Run: node test/mailbox_test.mjs   (from cloud/mailbox-worker, after npm ci)
import { spawn } from "node:child_process";
import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";

const PORT = 8796; // never 8797 (ChessTactics' test host)
const BASE = `http://127.0.0.1:${PORT}`;
let fail = 0;
const check = (ok, what) => { console.log((ok ? "ok   " : "FAIL ") + what); if (!ok) fail++; };

const dev = spawn(process.execPath, ["node_modules/wrangler/bin/wrangler.js", "dev", "--ip", "127.0.0.1", "--port", String(PORT)], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
let log = "";
dev.stdout.on("data", (d) => (log += d));
dev.stderr.on("data", (d) => (log += d));

async function ready() {
  for (let i = 0; i < 120; i++) {
    try { if ((await fetch(BASE + "/")).ok) return true; } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
}

const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const sha = (b) => createHash("sha256").update(b).digest();

function putBody(topic, version, blob) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const writer = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
  const msg = Buffer.concat([Buffer.from("kt/cloud-put/v1"), topic, u64(version), sha(blob)]);
  const sig = sign(null, msg, privateKey);
  let nonce = 0;
  for (;; nonce++) {
    const h = sha(Buffer.concat([Buffer.from("kt/cloud-pow/v1"), topic, writer, u64(version), u64(nonce)]));
    if (h[0] === 0 && h[1] === 0) break; // 16 leading zero bits
  }
  return Buffer.concat([writer, u64(version), u64(nonce), sig, blob]);
}

try {
  check(await ready(), "wrangler dev is up");
  const topic = randomBytes(32);
  const url = `${BASE}/v1/t/${topic.toString("hex")}`;

  const empty = await fetch(url);
  const eb = Buffer.from(await empty.arrayBuffer());
  check(empty.status === 200 && eb.length === 8 && eb.readBigUInt64LE() === 0n, "a never-written topic reads as empty"); // pinned: wire format rev u64

  const blob = Buffer.from("sealed bytes");
  const put = await fetch(url, { method: "PUT", body: putBody(topic, 1, blob) });
  check(put.status === 200 && (await put.text()) === "stored", "a signed put with proof of work is stored"); // pinned: wire format
  const got = Buffer.from(await (await fetch(url)).arrayBuffer());
  check(got.includes(blob), "and reads back");

  const bad = putBody(topic, 2, blob);
  bad[60] ^= 1; // break the signature
  check((await fetch(url, { method: "PUT", body: bad })).status === 400, "a bad signature is refused"); // pinned: wire contract the shipped apps read

  const big = await fetch(url, { method: "PUT", body: Buffer.alloc(112 + 640 * 1024 + 1) });
  check(big.status === 413, "an oversized put is refused from its length"); // pinned: wire contract (413 = slot too large)

  // A loop of reads from one address: the budget answers 429 well before 1000.
  let limited = 0;
  for (let i = 0; i < 1000 && !limited; i++) {
    const r = await fetch(`${BASE}/v1/t/${randomBytes(32).toString("hex")}`);
    if (r.status === 429) limited = i;
    else await r.arrayBuffer();
  }
  check(limited > 0, `a read loop is cut off (429 after ${limited} reads)`);
} finally {
  dev.kill();
}
if (fail) console.log(log.slice(-2000));
console.log(fail ? `FAIL ${fail}` : "mailbox OK");
process.exit(fail ? 1 : 0);
