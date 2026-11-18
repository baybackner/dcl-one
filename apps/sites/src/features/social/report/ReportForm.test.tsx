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

vi.mock("@/data/lib/auth/context", () => ({ useAuth: () => ({ identity: null, address: null, isConnected: false }) }));
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
});

describe("when a signed-out visitor fills the report form", () => {
  it("should open the sign-in on the page and keep what was typed", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <ReportForm trackCtx={{ sid: "sid", story: "report" } as never} />
        <SignInState />
      </MemoryRouter>,
    );
    const description = screen.getByPlaceholderText("Write your description here...");
    await user.type(description, "The player sent a phishing link");
    await user.click(screen.getByRole("button", { name: "Sign In" }));
    expect(screen.getByLabelText("Sign-in modal").textContent).toBe("open");
    expect(description).toHaveProperty("value", "The player sent a phishing link");
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
