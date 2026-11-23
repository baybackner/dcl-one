import { RouterContextProvider } from "react-router";
import { describe, expect, it } from "vitest";

import { sessionMiddleware } from "@/core/lib/telemetry/session";

import { loader } from "./internal.session";

const WALLET = "0x3333333333333333333333333333333333333333";

// The request as the server runs it: through the root session middleware, then this loader.
async function get(cookie = "") {
  const request = new Request("https://example.test/internal/session", { headers: { cookie } });
  const context = new RouterContextProvider();
  return (await sessionMiddleware(
    { request, context, params: {} } as never,
    async () => loader({ request, context, params: {} } as never),
  )) as Response;
}

describe("internal.session loader", () => {
  it("answers the visitor's existing sid and sets no cookie", async () => {
    const response = await get("sid=existing-sid");
    expect(await response.json()).toEqual({ sid: "existing-sid", userId: null });
    expect(response.headers.get("Set-Cookie")).toBeNull();
  });

  it("mints a sid for a visitor without one and sets its cookie", async () => {
    const response = await get();
    const { sid } = (await response.json()) as { sid: string };
    expect(sid).toMatch(/\S/);
    expect(response.headers.get("Set-Cookie")).toContain(`sid=${encodeURIComponent(sid)}`);
  });

  it("is never stored by a cache", async () => {
    expect((await get("sid=s")).headers.get("Cache-Control")).toBe("no-store, private");
  });

  it("answers the verified wallet only, not the display one", async () => {
    expect(await (await get(`sid=s; dcl_wallet=${WALLET}`)).json()).toEqual({ sid: "s", userId: null });
    expect(await (await get(`sid=s; dcl_wallet_verified=${WALLET}`)).json()).toEqual({ sid: "s", userId: WALLET });
  });

  it("sets the cookie itself when no middleware ran", async () => {
    const request = new Request("https://example.test/internal/session");
    const response = loader({ request, context: new RouterContextProvider(), params: {} } as never);
    const { sid } = (await response.json()) as { sid: string };
    expect(response.headers.get("Set-Cookie")).toContain(`sid=${encodeURIComponent(sid)}`);
  });
});
