import { generatePrivateKey, privateKeyToAccount } from "viem/accounts";
import { describe, expect, it } from "vitest";

import { createIdentityFromPrivateKey } from "./identity";
import { signRequest } from "./signer";
import { verifyAuthChainRequest } from "./verify-request.server";

async function signed(
  key: `0x${string}`,
  pathname: string,
  identityOpts: { expirationMs?: number } = {},
): Promise<{ request: Request; wallet: string }> {
  const identity = await createIdentityFromPrivateKey(key, identityOpts);
  const s = await signRequest(identity, "GET", pathname, {});
  return { request: new Request(`http://localhost${pathname}`, { headers: new Headers(s.headers) }), wallet: identity.signer };
}

async function signedWithEphemeralPayload(payload: (ephemeralAddress: string) => string): Promise<Request> {
  const key = generatePrivateKey();
  const identity = await createIdentityFromPrivateKey(key);
  const message = payload(identity.ephemeral.address);
  identity.authChain = [
    identity.authChain[0]!,
    { type: "ECDSA_EPHEMERAL", payload: message, signature: await privateKeyToAccount(key).signMessage({ message }) },
  ];
  const s = await signRequest(identity, "GET", "/api/example", {});
  return new Request("http://localhost/api/example", { headers: new Headers(s.headers) });
}

const IN_AN_HOUR = () => new Date(Date.now() + 3_600_000).toISOString();

describe("verifyAuthChainRequest ephemeral payload", () => {
  describe("when the payload has the shape @dcl/crypto writes and has not expired", () => {
    it.each([
      ["\\n", "\n"],
      ["\\r\\n", "\r\n"],
    ])("should accept it with %s line breaks", async (_name, eol) => {
      const request = await signedWithEphemeralPayload((address) =>
        ["Decentraland Login", `Ephemeral address: ${address}`, `Expiration: ${IN_AN_HOUR()}`].join(eol),
      );

      expect((await verifyAuthChainRequest(request)).ok).toBe(true);
    });
  });

  describe("when the expiration has passed", () => {
    it("should refuse it as an expired identity", async () => {
      const request = await signedWithEphemeralPayload((address) =>
        ["Decentraland Login", `Ephemeral address: ${address}`, "Expiration: 2020-01-20T22:57:11.334Z"].join("\n"),
      );

      expect(await verifyAuthChainRequest(request)).toEqual({ ok: false, status: 401, error: "identity expired" });
    });
  });

  describe("when the payload is not the three-line ephemeral message", () => {
    it.each([
      ["has no Expiration line", (a: string) => ["Decentraland Login", `Ephemeral address: ${a}`].join("\n")],
      ["has an unparseable expiration", (a: string) => ["Decentraland Login", `Ephemeral address: ${a}`, "Expiration: never"].join("\n")],
      ["names the address on the first line", (a: string) => [`Ephemeral address: ${a}`, "Decentraland Login", `Expiration: ${IN_AN_HOUR()}`].join("\n")],
      ["names the expiration on a later line", (a: string) => ["Decentraland Login", `Ephemeral address: ${a}`, "Note", `Expiration: ${IN_AN_HOUR()}`].join("\n")],
      ["puts the address after other text on its line", (a: string) => ["Decentraland Login", `Note. Ephemeral address: ${a}`, `Expiration: ${IN_AN_HOUR()}`].join("\n")],
    ])("should refuse one that %s as malformed", async (_name, payload) => {
      const request = await signedWithEphemeralPayload(payload);

      expect(await verifyAuthChainRequest(request)).toEqual({ ok: false, status: 401, error: "malformed ephemeral payload" });
    });
  });
});

const CHAIN = "x-identity-auth-chain-";

async function signedHeaders(method = "GET", metadata: Record<string, unknown> = {}): Promise<Headers> {
  const identity = await createIdentityFromPrivateKey(generatePrivateKey());
  return new Headers((await signRequest(identity, method, "/api/example", metadata)).headers);
}

function link(headers: Headers, index: number): Record<string, unknown> {
  return JSON.parse(headers.get(`${CHAIN}${index}`)!) as Record<string, unknown>;
}

function tampered(headers: Headers, change: (headers: Headers) => void, method = "GET"): Request {
  const copy = new Headers(headers);
  change(copy);
  return new Request("http://localhost/api/example", { method, headers: copy });
}

describe("verifyAuthChainRequest refusals", () => {
  describe("when the request deviates from a valid signed GET", () => {
    it.each<[string, (headers: Headers) => void, string]>([
      ["a timestamp that is not a number", (h) => h.set("x-identity-timestamp", "soon"), "invalid identity timestamp"],
      ["a link that is not JSON", (h) => h.set(`${CHAIN}1`, "{"), "malformed auth link 1"],
      ["a link without a signature", (h) => h.set(`${CHAIN}1`, JSON.stringify({ ...link(h, 1), signature: undefined })), "malformed auth link 1"],
      ["more than nine links", (h) => { for (let i = 3; i <= 9; i++) h.set(`${CHAIN}${i}`, h.get(`${CHAIN}2`)!); }, "auth chain too long"],
      ["only a SIGNER link", (h) => { h.delete(`${CHAIN}1`); h.delete(`${CHAIN}2`); }, "empty auth chain"],
      ["a first link that is not a SIGNER", (h) => h.set(`${CHAIN}0`, JSON.stringify({ ...link(h, 0), type: "ECDSA_EPHEMERAL" })), "missing SIGNER link"],
      ["a SIGNER that is not an address", (h) => h.set(`${CHAIN}0`, JSON.stringify({ ...link(h, 0), payload: "0x1234" })), "missing SIGNER link"],
      ["a signature that recovers no address", (h) => h.set(`${CHAIN}1`, JSON.stringify({ ...link(h, 1), signature: "0x00" })), "unrecoverable signature at link 1"],
      ["a delegation signed by a key other than the SIGNER", (h) => h.set(`${CHAIN}0`, JSON.stringify({ ...link(h, 0), payload: "0x1111111111111111111111111111111111111111" })), "broken auth chain at link 1"],
      ["an unsupported link type", (h) => h.set(`${CHAIN}2`, JSON.stringify({ ...link(h, 2), type: "ECDSA_EIP_1654_SIGNED_ENTITY" })), "unsupported auth link type ECDSA_EIP_1654_SIGNED_ENTITY"],
      ["no signed-request link", (h) => h.delete(`${CHAIN}2`), "missing signed request link"],
      ["tampered metadata", (h) => h.set("x-identity-metadata", JSON.stringify({ intent: "other" })), "request signature does not match request"],
      ["a link after the signed-request link", (h) => h.set(`${CHAIN}3`, h.get(`${CHAIN}2`)!), "link after the signed request link"],
    ])("should refuse %s with 401", async (_name, change, error) => {
      const request = tampered(await signedHeaders(), change);

      expect(await verifyAuthChainRequest(request)).toEqual({ ok: false, status: 401, error });
    });
  });

  describe("when the timestamp is too far in the future", () => {
    it("should refuse it as stale", async () => {
      const headers = await signedHeaders();
      const ts = Number(headers.get("x-identity-timestamp"));

      expect(await verifyAuthChainRequest(tampered(headers, () => {}), { now: ts - 20 * 60_000 })).toEqual({ ok: false, status: 401, error: "stale identity timestamp" });
    });
  });

  describe("when a POST signature is sent as a GET", () => {
    it("should refuse it as not matching the request", async () => {
      const request = tampered(await signedHeaders("POST"), () => {}, "GET");

      expect(await verifyAuthChainRequest(request)).toEqual({ ok: false, status: 401, error: "request signature does not match request" });
    });
  });

  describe("when a GET signature is sent as a HEAD", () => {
    it("should accept it", async () => {
      const request = tampered(await signedHeaders("GET"), () => {}, "HEAD");

      expect((await verifyAuthChainRequest(request)).ok).toBe(true);
    });
  });
});

describe("verifyAuthChainRequest", () => {
  it("verifies a real signed request, derives the wallet from the chain, and rejects an unsigned request with 401", async () => {
    const key = generatePrivateKey();
    const { request, wallet } = await signed(key, "/api/example");
    const res = await verifyAuthChainRequest(request);
    expect(res.ok).toBe(true);
    if (!res.ok) return;
    expect(res.wallet).toBe(wallet);
    expect(res.wallet).toBe(privateKeyToAccount(key).address.toLowerCase());

    const anon = await verifyAuthChainRequest(new Request("http://localhost/api/example"));
    expect(anon.ok).toBe(false);
    if (anon.ok) return;
    expect(anon.status).toBe(401);
  });

  it("rejects an expired ephemeral identity, a stale timestamp, and a signature replayed across paths with 401", async () => {
    const expired = await signed(generatePrivateKey(), "/api/example", { expirationMs: 1_000 });
    const expiredTs = Number(expired.request.headers.get("x-identity-timestamp"));
    const expiredRes = await verifyAuthChainRequest(expired.request, { now: expiredTs + 2_000 });
    expect(expiredRes.ok).toBe(false);
    if (expiredRes.ok) return;
    expect(expiredRes.status).toBe(401);
    expect(expiredRes.error).toContain("expired");

    const stale = await signed(generatePrivateKey(), "/api/example");
    const staleTs = Number(stale.request.headers.get("x-identity-timestamp"));
    const staleRes = await verifyAuthChainRequest(stale.request, { now: staleTs + 20 * 60_000 });
    expect(staleRes.ok).toBe(false);
    if (staleRes.ok) return;
    expect(staleRes.status).toBe(401);

    const { request } = await signed(generatePrivateKey(), "/api/example/one");
    const moved = new Request("http://localhost/api/example/two", { headers: request.headers });
    const replayRes = await verifyAuthChainRequest(moved);
    expect(replayRes.ok).toBe(false);
    if (replayRes.ok) return;
    expect(replayRes.status).toBe(401);
  });
});
