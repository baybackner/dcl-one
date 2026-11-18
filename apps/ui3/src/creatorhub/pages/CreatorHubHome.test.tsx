import { afterEach, describe, expect, test } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import CreatorHubHome from "./CreatorHubHome";

afterEach(() => cleanup());

describe("creator hub home", () => {
  test("shows no templates row, What's happening or Live from the network", () => {
    render(<CreatorHubHome chrome={false} />);
    expect(screen.queryByRole("region", { name: "Start from a template" })).toBeNull();
    expect(screen.queryByText("What's happening")).toBeNull();
    expect(screen.queryByText("Live from the network")).toBeNull();
    expect(screen.queryByText("See what's on")).toBeNull();
    expect(screen.queryByText("See all places")).toBeNull();
  });

  test("starts compact and exposes the regular and airy top spacings for comparison", () => {
    const { container, rerender } = render(<CreatorHubHome chrome={false} />);
    expect(container.querySelector(".chh")).toHaveClass("chh--top-compact");
    rerender(<CreatorHubHome chrome={false} topSpacing="airy" />);
    expect(container.querySelector(".chh")).toHaveClass("chh--top-airy");
    expect(container.querySelector(".chh")).not.toHaveClass("chh--top-compact");
  });
});
