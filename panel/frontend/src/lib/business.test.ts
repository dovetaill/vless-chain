import assert from "node:assert/strict";
import { expiringSoon, lowQuota, matchesLine, parseMoney, recentDays } from "./business";
import type { Line, PanelState } from "../types";

assert.equal(parseMoney("0"), 0);
assert.equal(parseMoney("12.01"), 1201);
assert.equal(parseMoney(" 9.9 "), 990);
assert.equal(parseMoney(""), null);
for (const input of ["-1", "1.001", "1e2", "Infinity", "9007199254740991"]) {
  assert.throws(() => parseMoney(input));
}
const days = recentDays(Date.parse("2026-01-01T16:00:00Z") / 1000, { "2026-01-02": [2, 3] });
assert.equal(days.length, 30);
assert.equal(days[0].date, "2025-12-04");
assert.deepEqual(days.at(-1), { date: "2026-01-02", upload: 2, download: 3 });

const line = {
  name: "东京代理", hops: [{ node_id: "node", port: 443 }],
  access: { host: "proxy.example", public_host: "", public_port: null, password: "secret", tls: { domain: "" } },
  rental: { customer_id: "customer", expires_at: 100 + 7 * 86400 },
  limits: { monthly_gib: 1 }, traffic: { upload: 0, download: 1024 ** 3 * 0.9 },
} as Line;
const state = { customers: [{ id: "customer", name: "客户甲" }], nodes: [{ id: "node", name: "Tokyo", host: "192.0.2.1" }] } as PanelState;
assert.equal(matchesLine(line, state, " 客户甲 TOKYO 443 "), true);
assert.equal(matchesLine(line, state, "proxy.example"), true);
assert.equal(matchesLine(line, state, "192.0.2.1"), true);
assert.equal(matchesLine(line, state, "secret"), false);
assert.equal(matchesLine(line, state, "不存在"), false);
assert.equal(matchesLine({ ...line, access: { protocol: "vless" } } as Line, state, "Tokyo 443"), true, "成员脱敏代理不需要 TLS 或凭据字段即可搜索");
assert.equal(expiringSoon(line, 100), true);
assert.equal(expiringSoon(line, line.rental!.expires_at), false);
assert.equal(lowQuota(line), true);
assert.equal(lowQuota({ ...line, traffic: null }), false);
assert.equal(lowQuota({ ...line, limits: { ...line.limits, monthly_gib: null } }), false);

// Charged quota uses the multiplier; raw transfer totals remain unchanged.
assert.equal(lowQuota({...line, traffic:{...line.traffic!, upload:1, download:1, charged:1024 ** 3}}),true);
assert.equal(lowQuota({...line, traffic:{...line.traffic!, upload:1024 ** 3, download:0, charged:0}}),false);
