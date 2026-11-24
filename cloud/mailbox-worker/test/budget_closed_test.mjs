// machine-queue: light
// The day budget fails closed (safety audit 2026-11-24, S3-2 remainder; APP-RULE 71): a deploy that lost its BUDGET
// binding, or a budget object that errors, must not mean "unlimited". The per-minute limits already answer 503
// "limits missing" without their binding (round 5, S10); the day budget now does the same, and an error from the
// budget object counts as over (429: the apps defer to their other layers).
// Run: node test/budget_closed_test.mjs   (no wrangler: cloudflare:workers is stubbed)
// pinned: 503 / 429 and their texts are the wire contract the shipped apps read; 100 = the worker's REPORT batch
import { register } from "node:module";

const stub = "export class DurableObject { constructor(s, e) { this.ctx = s; this.env = e; } }";
const hooks = `export async function resolve(s, c, next) { return s === "cloudflare:workers" ? { url: "data:text/javascript,${encodeURIComponent(stub)}", shortCircuit: true } : next(s, c); }`;
register("data:text/javascript," + encodeURIComponent(hooks));
const worker = (await import("../src/index.js")).default;

let fail = 0;
const check = (ok, what) => { console.log((ok ? "ok   " : "FAIL ") + what); if (!ok) fail++; };
const limits = { limit: async () => ({ success: true }) };
const topic = { idFromName: (n) => n, get: () => ({ fetch: async () => new Response(new Uint8Array(0)) }) };
const url = "https://m.example/v1/t/" + "ab".repeat(32);
const get = (env, ip) => worker.fetch(new Request(url, { headers: { "CF-Connecting-IP": ip } }), env);

// no BUDGET binding: refused like the missing per-minute limits
const r1 = await get({ READS: limits, WRITES: limits, TOPICS: topic }, "203.0.113.7");
check(r1.status === 503, `no day budget binding answers 503, not unlimited (got ${r1.status})`); // pinned: 503 = limits missing (wire contract)

// a budget object that errors: over (429) when it is asked, i.e. on the 100th request of a network
const broken = { idFromName: (n) => n, get: () => ({ fetch: async () => { throw new Error("object down"); } }) };
let last = null;
for (let i = 0; i < 100; i++) last = await get({ READS: limits, WRITES: limits, TOPICS: topic, BUDGET: broken }, "198.51.100.9");
check(last.status === 429, `a budget object that errors counts as over (got ${last.status})`); // pinned: 429 = defer to other layers (wire contract)

console.log(fail ? `FAIL ${fail}` : "budget closed OK");
process.exit(fail ? 1 : 0);
