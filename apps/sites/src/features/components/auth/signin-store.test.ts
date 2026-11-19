// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { installHostSignIn, openSignIn } from "./signin-store";

const assign = vi.fn();

beforeEach(() => {
  vi.stubGlobal("location", { pathname: "/shop/items", search: "?sortBy=newest", hash: "#grid", assign });
});
afterEach(() => {
  vi.unstubAllGlobals();
  assign.mockClear();
  delete (window as { __DCL_SHELL__?: unknown }).__DCL_SHELL__;
});

describe("when a visitor requests sign-in", () => {
  it("should return to the current path, query and hash", () => {
    openSignIn();
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Fshop%2Fitems%3FsortBy%3Dnewest%23grid");
  });

  it("should preserve the return path supplied by the caller", () => {
    openSignIn({ redirectTo: "/shop/checkout" });
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Fshop%2Fcheckout");
  });

  it("should ask the embedded Explorer host to handle sign-in", () => {
    const hostSignIn = vi.fn();
    const uninstall = installHostSignIn(hostSignIn);

    try {
      openSignIn();
      expect(hostSignIn).toHaveBeenCalledOnce();
      expect(assign).not.toHaveBeenCalled();
    } finally {
      uninstall();
    }
  });

  it("should send the desktop shell to the sign-in page", () => {
    (window as { __DCL_SHELL__?: unknown }).__DCL_SHELL__ = { shell: "tauri" };
    openSignIn({ redirectTo: "/create/storage?step=edit" });
    expect(assign).toHaveBeenCalledWith("/auth/login?redirectTo=%2Fcreate%2Fstorage%3Fstep%3Dedit");
  });
});
