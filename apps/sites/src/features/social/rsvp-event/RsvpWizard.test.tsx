// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

import RsvpWizard, { type RsvpEventView } from "./RsvpWizard";

const EVENT: RsvpEventView = {
  id: "e1",
  title: "Poker Night",
  when: "FRI, NOV 20 @ 8:00PM",
  host: "OtoLam",
  description: "Cards and chips.",
  schedule: "Friday, Nov 20 from 8:00pm to 10:00pm",
  location: "(10,20)",
  jumpHref: "/play/?position=10%2C20",
};

afterEach(cleanup);

function setup() {
  const onSignIn = vi.fn();
  const router = createMemoryRouter(
    [
      {
        path: "/events/:id/rsvp",
        element: <RsvpWizard trackCtx={{ sid: "sid", story: "landings/rsvp-event" } as never} event={EVENT} signedIn={false} onSignIn={onSignIn} track={vi.fn()} />,
      },
    ],
    { initialEntries: ["/events/e1/rsvp"] },
  );
  render(<RouterProvider router={router} />);
  return { router, onSignIn, user: userEvent.setup() };
}

const overlay = () => document.querySelector(".evd__backdrop");

describe("when the RSVP event detail is dismissed", () => {
  it("should stay dismissed while the RSVP moves to its next step", async () => {
    const { router, user } = setup();
    expect(overlay()).not.toBeNull();
    await user.click(screen.getByRole("button", { name: "Close", hidden: true }));
    expect(overlay()).toBeNull();
    await user.click(screen.getByRole("button", { name: "Going" }));
    expect(router.state.location.search).toBe("?step=signin-gate");
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeTruthy();
    expect(overlay()).toBeNull();
  });

  it("should send a signed-out visitor to sign in from the gate, with the detail out of the way", async () => {
    const { onSignIn, user } = setup();
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Going" }));
    await user.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(onSignIn).toHaveBeenCalledOnce();
    expect(overlay()).toBeNull();
  });
});
