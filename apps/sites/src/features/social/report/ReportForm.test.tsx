// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, within } from "@testing-library/react";
import { ChromeAuthContext } from "@dcl/one-ui/components/chrome/chrome-auth";
import { CREDITS_BALANCE_QUERY_KEY } from "@shop/credits/balance-query";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { openSignIn } from "../../components/auth/signin-store";
import ReportForm from "./ReportForm";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

const auth = vi.hoisted(() => ({ value: { identity: null as unknown, address: null as string | null, isConnected: false } }));
vi.mock("@/data/lib/auth/context", () => ({ useAuth: () => auth.value }));
vi.mock("@dcl/one-ui/apps/sites/shared/AnimatedBackground/AnimatedBackground", () => ({
  AnimatedBackground: () => <div data-testid="animated-background" />,
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  auth.value = { identity: null, address: null, isConnected: false };
});

const signer = "0x1111111111111111111111111111111111111111";

function renderReport(url = "/report") {
  const queryClient = new QueryClient();
  queryClient.setQueryData([CREDITS_BALANCE_QUERY_KEY, signer], { credits: 0 });
  const form = () => (
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[url]}>
        <ChromeAuthContext.Provider
          value={{
            signedIn: auth.value.isConnected,
            account: auth.value.address ?? "",
            name: "",
            mana: "",
            committee: false,
            onSignIn: openSignIn,
          }}
        >
          <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
        </ChromeAuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  );
  const view = render(form());
  return { ...view, updateSession: () => view.rerender(form()) };
}

function signInButton(entry: "header" | "form") {
  return within(screen.getByRole(entry === "header" ? "banner" : "main")).getByRole("button", { name: /^sign in$/i });
}

describe.each(["header", "form"] as const)("when a visitor requests sign-in from the %s", (entry) => {
  it("should restore the written draft after sign-in and then allow evidence to be attached", async () => {
    const user = userEvent.setup();
    const url = "/report?player_address=0x1111111111111111111111111111111111111111";
    const reported = "0x2222222222222222222222222222222222222222";
    const first = renderReport(url);
    await user.type(screen.getByPlaceholderText("Write or paste an address here..."), reported);
    await user.type(screen.getByPlaceholderText("Write your description here..."), "Report text");
    await user.type(screen.getByPlaceholderText("Write your comments here..."), "Report comment");
    expect(first.container.querySelector('input[type="file"]')).toBeNull();
    vi.stubGlobal("location", { pathname: "/report", search: url.slice(url.indexOf("?")), hash: "", assign: vi.fn() });
    await user.click(signInButton(entry));
    first.unmount();

    auth.value = { identity: { signer }, address: signer, isConnected: true };
    const next = renderReport(url);

    expect((screen.getByPlaceholderText("Write or paste an address here...") as HTMLInputElement).value).toBe(reported);
    expect((screen.getByPlaceholderText("Write your description here...") as HTMLTextAreaElement).value).toBe("Report text");
    expect((screen.getByPlaceholderText("Write your comments here...") as HTMLTextAreaElement).value).toBe("Report comment");
    const fileInput = next.container.querySelector<HTMLInputElement>('input[type="file"]')!;
    await user.upload(fileInput, new File(["image"], "evidence.png", { type: "image/png" }));
    expect(screen.getByText("evidence.png")).toBeTruthy();
    expect(sessionStorage.getItem("report_form_draft")).toBeNull();
  });

  it("should send them to the sign-in page, to come back to this report, with no modal", async () => {
    const user = userEvent.setup();
    renderReport();
    const button = signInButton(entry);
    const assign = vi.fn();
    vi.stubGlobal("location", { pathname: "/report-user", search: "?player=0xab", hash: "", assign });
    await user.click(button);
    vi.unstubAllGlobals();
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Freport-user%3Fplayer%3D0xab");
  });

  it("should keep a written draft on the page when the browser refuses to save it", async () => {
    const user = userEvent.setup();
    renderReport();
    const description = screen.getByPlaceholderText<HTMLTextAreaElement>("Write your description here...");
    await user.type(description, "Report text");
    const assign = vi.fn();
    vi.stubGlobal("location", { pathname: "/report", search: "", hash: "", assign });
    vi.stubGlobal("sessionStorage", undefined);
    await user.click(signInButton(entry));

    expect(assign).not.toHaveBeenCalled();
    expect(description.value).toBe("Report text");
    expect(screen.getByRole("alert").textContent).toContain("couldn’t save your draft");

    await user.clear(description);
    await user.click(signInButton(entry));
    expect(assign).toHaveBeenCalledExactlyOnceWith("/auth/login?redirectTo=%2Freport");
  });

  it("should keep the files visible after logout and require confirmation before leaving them", async () => {
    const user = userEvent.setup();
    auth.value = { identity: { signer }, address: signer, isConnected: true };
    const view = renderReport();
    const file = new File(["image"], "evidence.png", { type: "image/png" });
    await user.upload(view.container.querySelector<HTMLInputElement>('input[type="file"]')!, file);

    auth.value = { identity: null, address: null, isConnected: false };
    view.updateSession();
    expect(screen.getByText(file.name)).toBeTruthy();
    expect(view.container.querySelector('input[type="file"]')).toBeNull();

    const assign = vi.fn();
    vi.stubGlobal("location", { pathname: "/report", search: "", hash: "", assign });
    await user.click(signInButton(entry));
    expect(assign).not.toHaveBeenCalled();
    expect(screen.getByText(/attach them again after signing in/i)).toBeTruthy();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    expect(document.activeElement).toBe(cancel);
    await user.click(cancel);
    expect(screen.getByText(file.name)).toBeTruthy();
    expect(document.activeElement).toBe(signInButton(entry));
    expect(assign).not.toHaveBeenCalled();

    await user.click(signInButton(entry));
    await user.click(screen.getByRole("button", { name: "Continue to sign in" }));
    expect(assign).toHaveBeenCalledExactlyOnceWith("/auth/login?redirectTo=%2Freport");
  });
});

describe("when a signed-in visitor opens the report form", () => {
  it("should not offer to sign in", () => {
    auth.value = { identity: { signer }, address: signer, isConnected: true };
    renderReport();
    expect(screen.queryByRole("button", { name: /^sign in$/i })).toBeNull();
    expect(screen.queryByText("Please sign in with your wallet to submit a report.")).toBeNull();
  });
});

describe("when the report form renders", () => {
  it("should draw the animated background once, behind the form", () => {
    renderReport();
    const backgrounds = screen.getAllByTestId("animated-background");
    expect(backgrounds).toHaveLength(1);
    const description = screen.getByPlaceholderText("Write your description here...");
    expect(backgrounds[0]!.contains(description)).toBe(false);
  });
});
