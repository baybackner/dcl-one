// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DayCard, UpcomingCard } from "@dcl/one-ui/apps/sites/events/cards";
vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);
const toggle = vi.fn();
const copy = vi.fn();
const props = {
  id: "event",
  title: "Poker Night",
  image: null,
  href: "/events?id=event",
  creator: { name: "Host", href: "/profile/host" },
  time: { label: "Tomorrow", dateTime: "2026-11-13T12:00:00Z" },
  remind: { active: false, pending: false, onToggle: toggle },
  calendarHref: "https://calendar.google.com/calendar/render?action=TEMPLATE",
};
let user: ReturnType<typeof userEvent.setup>;
beforeEach(() => {
  // Set up before the clipboard stub below, so the stub, not user-event's own clipboard, is what the card writes to.
  user = userEvent.setup();
  toggle.mockReset();
  copy.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText: copy } });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
describe("when event cards reveal their actions", () => {
  it.each(["upcoming", "day"])(
    "should keep the %s card time and all three independent actions mounted",
    (kind) => {
      render(
        kind === "upcoming" ? (
          <UpcomingCard {...props} />
        ) : (
          <DayCard {...props} live={false} />
        ),
      );
      expect(screen.getByText("Tomorrow").closest("time")?.dateTime).toBe(
        props.time.dateTime,
      );
      expect(
        screen.getByRole("button", { name: "Remind me: Poker Night" }),
      ).toBeDefined();
      expect(
        screen
          .getByRole("link", { name: "Add to calendar: Poker Night" })
          .getAttribute("href"),
      ).toBe(props.calendarHref);
      expect(
        screen
          .getByRole("button", { name: "Copy link: Poker Night" })
          .closest("a"),
      ).toBeNull();
    },
  );
  it("should render the upcoming card with its host and icon-only actions", () => {
    render(<UpcomingCard {...props} />);
    const card = screen.getByRole("article");
    expect(card.textContent).toContain("by Host");
    for (const name of ["Remind me: Poker Night", "Copy link: Poker Night"])
      expect(screen.getByRole("button", { name }).textContent).toBe("");
    expect(screen.queryByText("Remind me")).toBeNull();
  });
  it("should mark an active reminder as pressed and disable it while pending", async () => {
    render(
      <UpcomingCard
        {...props}
        remind={{ active: true, pending: true, onToggle: toggle }}
      />,
    );
    const button = screen.getByRole("button", {
      name: "Remind me: Poker Night",
    }) as HTMLButtonElement;
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(button.disabled).toBe(true);
    await user.click(button);
    expect(toggle).not.toHaveBeenCalled();
  });
  it("should leave out Remind me and Add to calendar when the page offers neither", () => {
    render(
      <UpcomingCard {...props} remind={undefined} calendarHref={undefined} />,
    );
    expect(screen.queryByRole("button", { name: /Remind me/ })).toBeNull();
    expect(screen.queryByRole("link", { name: /Add to calendar/ })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    ).toBeDefined();
  });
  it("should show a live event's badges, JUMP IN and Copy link instead of Remind me", async () => {
    const onJumpIn = vi.fn((event: { preventDefault: () => void }) =>
      event.preventDefault(),
    );
    const open = vi.fn();
    render(
      <UpcomingCard
        {...props}
        remind={undefined}
        live
        users={16}
        usersLabel="16 online"
        jumpIn={{ href: "/play?position=1,1", onJumpIn }}
        onOpen={open}
      />,
    );
    expect(screen.getByText("LIVE")).toBeDefined();
    expect(screen.getByText("16 online")).toBeDefined();
    const jump = screen.getByRole("link", { name: "Jump in: Poker Night" });
    expect(jump.getAttribute("href")).toBe("/play?position=1,1");
    await user.click(jump);
    expect(onJumpIn).toHaveBeenCalledOnce();
    expect(open).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: /Remind me/ })).toBeNull();
    expect(screen.queryByRole("link", { name: /Add to calendar/ })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    ).toBeDefined();
  });
  describe("and a live event is in a desktop day column", () => {
    const href = "/play?realm=gallery.dcl.eth";
    const onJumpIn = vi.fn((event: { preventDefault: () => void }) =>
      event.preventDefault(),
    );
    const onOpen = vi.fn();

    beforeEach(() => {
      onJumpIn.mockClear();
      onOpen.mockClear();
    });

    it("should expose Jump In independently of the detail link", async () => {
      render(
        <DayCard
          {...props}
          live
          users={0}
          jumpIn={{ href, onJumpIn }}
          onOpen={onOpen}
        />,
      );
      const link = screen.getByRole("link", { name: "Jump in: Poker Night" });
      expect(link.getAttribute("href")).toBe(href);
      await user.click(link);
      expect(onJumpIn).toHaveBeenCalledOnce();
      expect(onOpen).not.toHaveBeenCalled();
    });

    it("should offer Copy link even if no launch destination was supplied", () => {
      render(<DayCard {...props} live />);
      expect(screen.queryByRole("link", { name: /Jump in/ })).toBeNull();
      expect(
        screen.getByRole("button", { name: "Copy link: Poker Night" }),
      ).toBeDefined();
    });
  });
  it("should keep a live event without a jump target to Copy link, with no unknown count", () => {
    render(<UpcomingCard {...props} remind={undefined} live image={null} />);
    expect(screen.queryByRole("link", { name: /Jump in/ })).toBeNull();
    expect(screen.getByText("LIVE")).toBeDefined();
    expect(screen.queryByText(/online/)).toBeNull();
    expect(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    ).toBeDefined();
  });
  it("should render a card with no host and no image", () => {
    render(<UpcomingCard {...props} creator={undefined} image={null} />);
    expect(screen.getByRole("article").textContent).not.toContain("by ");
    expect(screen.getByRole("link", { name: "Poker Night" })).toBeDefined();
  });
  it("should toggle a reminder without opening the card", async () => {
    const open = vi.fn();
    render(<UpcomingCard {...props} onOpen={open} />);
    await user.click(
      screen.getByRole("button", { name: "Remind me: Poker Night" }),
    );
    expect(toggle).toHaveBeenCalledOnce();
    expect(open).not.toHaveBeenCalled();
  });
  it("should copy the actual card link without navigating", async () => {
    const open = vi.fn();
    render(<UpcomingCard {...props} onOpen={open} />);
    await user.click(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toBe("Copied"),
    );
    expect(copy).toHaveBeenCalledWith(
      new URL(props.href, window.location.href).href,
    );
    expect(open).not.toHaveBeenCalled();
  });
  it("should report a clipboard failure without changing the reminder", async () => {
    copy.mockRejectedValue(new Error("denied"));
    render(<UpcomingCard {...props} />);
    await user.click(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("Couldn't copy"),
    );
    expect(toggle).not.toHaveBeenCalled();
  });
  it("should handle a missing clipboard API", async () => {
    vi.stubGlobal("navigator", {});
    render(<UpcomingCard {...props} />);
    await user.click(
      screen.getByRole("button", { name: "Copy link: Poker Night" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("Couldn't copy"),
    );
  });
  it("should recover when copying succeeds after a denied attempt", async () => {
    copy.mockRejectedValueOnce(new Error("denied"));
    render(<UpcomingCard {...props} />);
    const button = screen.getByRole("button", {
      name: "Copy link: Poker Night",
    });
    await user.click(button);
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("Couldn't copy"),
    );
    await user.click(button);
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toBe("Copied"),
    );
  });
});
