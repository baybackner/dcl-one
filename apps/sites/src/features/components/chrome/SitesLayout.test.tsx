// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChromeAuthContext } from "@dcl/one-ui/components/chrome/chrome-auth";
import { createShopClient } from "@shop/api/shop";
import { CREDITS_BALANCE_QUERY_KEY } from "@shop/credits/balance-query";
import { sitesShopClient } from "@/data/lib/auth/shop-client";

import SitesLayout from "./SitesLayout";

vi.mock("@/data/lib/auth/shop-client", () => ({ sitesShopClient: vi.fn() }));

vi.hoisted(() => Object.assign(window, {
  __vite_plugin_react_preamble_installed__: true,
  $RefreshSig$: () => (type: unknown) => type,
  $RefreshReg$: () => {},
}));

afterEach(cleanup);

const account = "0x000000000000000000000000000000000000abcd";

describe("when Sites renders the credits chip", () => {
  let queryClient: QueryClient;
  const transport = vi.fn();

  beforeEach(() => {
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    transport.mockReset();
    vi.mocked(sitesShopClient).mockReturnValue(createShopClient({
      base: "https://sites.test",
      mode: "canonical",
      signer: { address: account, sign: async () => ({}) },
      transport,
    }));
  });

  function show(signedIn: boolean) {
    render(
      <MemoryRouter initialEntries={["/events"]}>
        <QueryClientProvider client={queryClient}>
          <ChromeAuthContext value={{ signedIn, account: signedIn ? account : "", mana: "", name: "", committee: false }}>
            <SitesLayout>Events</SitesLayout>
          </ChromeAuthContext>
        </QueryClientProvider>
      </MemoryRouter>,
    );
  }

  it("should skip balance requests for a visitor", () => {
    show(false);

    expect(transport).not.toHaveBeenCalled();
  });

  it("should show the signed-in balance and share the Shop account key", async () => {
    transport.mockResolvedValueOnce({ status: 200, body: JSON.stringify({ usd: { balanceCents: 12340 } }) });
    show(true);

    expect((await screen.findByRole("link", { name: "1,234 credits" })).getAttribute("href")).toBe("/shop/credits");
    expect(queryClient.getQueryData([CREDITS_BALANCE_QUERY_KEY, account])).toMatchObject({ credits: 1234 });
  });

  it("should hide the balance when the upstream request fails", async () => {
    transport.mockResolvedValueOnce({ status: 503, body: "{}" });
    show(true);

    await waitFor(() => expect(queryClient.getQueryState([CREDITS_BALANCE_QUERY_KEY, account])?.status).toBe("error"));
    expect(screen.queryByRole("link", { name: /credits/i })).toBeNull();
  });
});
