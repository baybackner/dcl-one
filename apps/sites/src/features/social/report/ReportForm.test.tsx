// @vitest-environment jsdom
import type { ReactNode } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

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
vi.mock("../../components/chrome/SitesLayout", () => ({ default: ({ children }: { children?: ReactNode }) => <>{children}</> }));
vi.mock("@dcl/one-ui/apps/sites/shared/AnimatedBackground/AnimatedBackground", () => ({
  AnimatedBackground: () => <div data-testid="animated-background" />,
}));

afterEach(() => {
  cleanup();
  sessionStorage.clear();
  vi.unstubAllGlobals();
  auth.value = { identity: null, address: null, isConnected: false };
});

describe("when a signed-out visitor fills the report form", () => {
  it("should restore the written draft after sign-in and then allow evidence to be attached", async () => {
    const user = userEvent.setup();
    const url = "/report?player_address=0x1111111111111111111111111111111111111111";
    const reported = "0x2222222222222222222222222222222222222222";
    const showReport = () => render(
      <MemoryRouter initialEntries={[url]}>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
      </MemoryRouter>,
    );
    const first = showReport();
    await user.type(screen.getByPlaceholderText("Write or paste an address here..."), reported);
    await user.type(screen.getByPlaceholderText("Write your description here..."), "Report text");
    await user.type(screen.getByPlaceholderText("Write your comments here..."), "Report comment");
    expect(first.container.querySelector('input[type="file"]')).toBeNull();
    vi.stubGlobal("location", { pathname: "/report", search: url.slice(url.indexOf("?")), hash: "", assign: vi.fn() });
    await user.click(screen.getByRole("button", { name: "Sign In" }));
    first.unmount();

    const signer = "0x1111111111111111111111111111111111111111";
    auth.value = { identity: { signer }, address: signer, isConnected: true };
    const next = showReport();

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
    render(
      <MemoryRouter>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
      </MemoryRouter>,
    );
    const button = screen.getByRole("button", { name: "Sign In" });
    const assign = vi.fn();
    vi.stubGlobal("location", { pathname: "/report-user", search: "?player=0xab", hash: "", assign });
    await user.click(button);
    vi.unstubAllGlobals();
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Freport-user%3Fplayer%3D0xab");
  });
});

describe("when a signed-in visitor opens the report form", () => {
  it("should not offer to sign in", () => {
    const signer = "0x1111111111111111111111111111111111111111";
    auth.value = { identity: { signer }, address: signer, isConnected: true };
    render(
      <MemoryRouter>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
      </MemoryRouter>,
    );
    expect(screen.queryByRole("button", { name: "Sign In" })).toBeNull();
    expect(screen.queryByText("Please sign in with your wallet to submit a report.")).toBeNull();
  });
});

describe("when the report form renders", () => {
  it("should draw the animated background once, behind the form", () => {
    render(
      <MemoryRouter>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
      </MemoryRouter>,
    );
    const backgrounds = screen.getAllByTestId("animated-background");
    expect(backgrounds).toHaveLength(1);
    const description = screen.getByPlaceholderText("Write your description here...");
    expect(backgrounds[0]!.contains(description)).toBe(false);
  });
});
