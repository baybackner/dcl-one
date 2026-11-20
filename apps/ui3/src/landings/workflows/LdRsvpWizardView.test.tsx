import { afterEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import LdRsvpWizardView from "./LdRsvpWizardView";

const EVENT = {
  title: "Poker Night",
  when: "FRI, NOV 20 @ 8:00PM",
  host: "OtoLam",
  description: "Cards and chips.",
  schedule: "Friday, Nov 20 from 8:00pm to 10:00pm",
  location: "(10,20)",
  jumpHref: "/play/?position=10%2C20",
};

afterEach(cleanup);

const overlay = () => document.querySelector(".evd__backdrop");

describe("LdRsvpWizardView", () => {
  test("opens with the event detail over the RSVP panel", () => {
    render(<LdRsvpWizardView event={EVENT} />);
    expect(overlay()).not.toBeNull();
    expect(screen.getByRole("heading", { name: "Poker Night" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Event details" })).toBeNull();
  });

  test("Close dismisses the detail, focuses the panel and leaves Going reachable", async () => {
    const onTapGoing = vi.fn();
    const user = userEvent.setup();
    render(<LdRsvpWizardView event={EVENT} onTapGoing={onTapGoing} />);
    await user.click(screen.getByRole("button", { name: "Close", hidden: true }));
    expect(overlay()).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("group", { name: "RSVP to this event" }));
    await user.click(screen.getByRole("button", { name: "Going" }));
    expect(onTapGoing).toHaveBeenCalledOnce();
  });

  test("a click on the backdrop dismisses the detail, a click inside it does not", async () => {
    const user = userEvent.setup();
    render(<LdRsvpWizardView event={EVENT} />);
    await user.click(screen.getByRole("heading", { name: "Poker Night" }));
    expect(overlay()).not.toBeNull();
    await user.click(overlay() as HTMLElement);
    expect(overlay()).toBeNull();
  });

  test("Escape dismisses the detail, and other keys do not", async () => {
    const user = userEvent.setup();
    render(<LdRsvpWizardView event={EVENT} />);
    await user.keyboard("{Enter}");
    expect(overlay()).not.toBeNull();
    await user.keyboard("{Escape}");
    expect(overlay()).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("group", { name: "RSVP to this event" }));
  });

  test("Event details reopens a dismissed detail", async () => {
    const user = userEvent.setup();
    render(<LdRsvpWizardView event={EVENT} />);
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Event details" }));
    expect(overlay()).not.toBeNull();
    expect(screen.queryByRole("button", { name: "Event details" })).toBeNull();
  });

  test("keeps the panel's step actions after the detail closes", async () => {
    const onSignIn = vi.fn();
    const user = userEvent.setup();
    render(<LdRsvpWizardView event={EVENT} value="signinGate" onSignIn={onSignIn} />);
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    expect(onSignIn).toHaveBeenCalledOnce();
  });

  test("follows a controlled detailOpen and reports Close and Escape to its owner", async () => {
    const onDetailOpenChange = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(
      <LdRsvpWizardView event={EVENT} detailOpen onDetailOpenChange={onDetailOpenChange} />,
    );
    await user.click(screen.getByRole("button", { name: "Close", hidden: true }));
    expect(onDetailOpenChange).toHaveBeenLastCalledWith(false);
    await user.keyboard("{Escape}");
    expect(onDetailOpenChange).toHaveBeenCalledTimes(2);
    rerender(<LdRsvpWizardView event={EVENT} detailOpen={false} onDetailOpenChange={onDetailOpenChange} />);
    expect(overlay()).toBeNull();
    await user.click(screen.getByRole("button", { name: "Event details" }));
    expect(onDetailOpenChange).toHaveBeenLastCalledWith(true);
  });
});
