// @vitest-environment jsdom
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

import { useSignedObjectUrls } from "./useSignedObjectUrls";

const DRAFT = "https://node.test/builder-api/v1/storage/contents/bafdraft";
const OTHER = "https://node.test/builder-api/v1/storage/contents/bafother";

describe("when a signed fetch reads the private files of a list", () => {
  let signedFetch: ReturnType<typeof vi.fn>;
  let createObjectURL: ReturnType<typeof vi.fn>;
  let revokeObjectURL: ReturnType<typeof vi.fn>;
  let inFlight: number;
  let mostInFlight: number;

  beforeEach(() => {
    inFlight = 0;
    mostInFlight = 0;
    signedFetch = vi.fn(async (url: string) => {
      inFlight += 1;
      mostInFlight = Math.max(mostInFlight, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 5));
      inFlight -= 1;
      return url.endsWith("missing") ? new Response("", { status: 404 }) : new Response(`bytes of ${url}`);
    });
    let next = 0;
    createObjectURL = vi.fn(() => `blob:https://node.test/${(next += 1)}`);
    revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", Object.assign(URL, { createObjectURL, revokeObjectURL }));
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  describe("and two files are wanted", () => {
    let result: { current: ReadonlyMap<string, string> };

    beforeEach(async () => {
      ({ result } = renderHook(() => useSignedObjectUrls([DRAFT, OTHER], signedFetch as unknown as typeof fetch)));
      await waitFor(() => expect(result.current.size).toBe(2));
    });

    it("should answer each file with an object URL of its bytes", () => {
      expect([...result.current.keys()].sort()).toEqual([DRAFT, OTHER]);
    });

    it("should keep the private bytes out of the HTTP cache", () => {
      expect(signedFetch.mock.calls.map(([, init]) => init.cache)).toEqual(["no-store", "no-store"]);
    });
  });

  describe("and a file answers a refusal", () => {
    let result: { current: ReadonlyMap<string, string> };

    beforeEach(async () => {
      ({ result } = renderHook(() =>
        useSignedObjectUrls([DRAFT, `${OTHER}missing`], signedFetch as unknown as typeof fetch),
      ));
      await waitFor(() => expect(result.current.size).toBe(1));
    });

    it("should leave the refused file without an object URL", () => {
      expect(result.current.has(`${OTHER}missing`)).toBe(false);
    });
  });

  describe("and twelve files are wanted at once", () => {
    beforeEach(async () => {
      const urls = Array.from({ length: 12 }, (_, i) => `${DRAFT}${i}`);
      const { result } = renderHook(() => useSignedObjectUrls(urls, signedFetch as unknown as typeof fetch));
      await waitFor(() => expect(result.current.size).toBe(12));
    });

    it("should fetch at most four at a time", () => {
      expect(mostInFlight).toBe(4);
    });
  });

  describe("and the list stops showing them", () => {
    beforeEach(async () => {
      const { result, unmount } = renderHook(() => useSignedObjectUrls([DRAFT, OTHER], signedFetch as unknown as typeof fetch));
      await waitFor(() => expect(result.current.size).toBe(2));
      unmount();
    });

    it("should revoke every object URL it made", () => {
      expect(revokeObjectURL.mock.calls.map(([url]) => url).sort()).toEqual(createObjectURL.mock.results.map((r) => r.value).sort());
    });
  });
});
