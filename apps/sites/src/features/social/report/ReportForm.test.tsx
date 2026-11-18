// @vitest-environment jsdom
import type { ReactNode } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { closeSignIn, useSignInOpen } from "../../components/auth/signin-store";
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
vi.mock("@ui/web/frames/SitesChrome", () => ({ default: ({ children }: { children?: ReactNode }) => <>{children}</> }));
vi.mock("@dcl/one-ui/apps/sites/shared/AnimatedBackground/AnimatedBackground", () => ({
  AnimatedBackground: () => <div data-testid="animated-background" />,
}));

function SignInState() {
  return <output aria-label="Sign-in modal">{useSignInOpen() ? "open" : "closed"}</output>;
}

afterEach(() => {
  closeSignIn();
  cleanup();
  auth.value = { identity: null, address: null, isConnected: false };
});

describe("when a signed-out visitor fills the report form", () => {
  it("should send them to the sign-in page, to come back to this report, with no modal", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
        <SignInState />
      </MemoryRouter>,
    );
    const button = screen.getByRole("button", { name: "Sign In" });
    const assign = vi.fn();
    vi.stubGlobal("location", { pathname: "/report-user", search: "?player=0xab", hash: "", assign });
    await user.click(button);
    vi.unstubAllGlobals();
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Freport-user%3Fplayer%3D0xab");
    expect(screen.getByLabelText("Sign-in modal").textContent).toBe("closed");
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
