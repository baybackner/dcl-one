import { beforeEach, describe, expect, it } from "vitest";

import { MetaTransactionError } from "../lib/metaTx";
import { writeFailureText } from "./kit";

describe("when a market write fails", () => {
  describe("and the relay refused it for its request limit", () => {
    let error: MetaTransactionError;

    beforeEach(() => {
      error = new MetaTransactionError("Too many requests. Retry in 2 seconds.", "api_limit_reached", 429, true, { error: "api_limit_reached", retryAt: 1 });
    });

    it("should tell the seller to wait a few seconds and try again", () => {
      expect(writeFailureText(error)).toBe("Too many requests from your connection. Wait a few seconds and try again.");
    });
  });

  describe("and the relay refused it for another reason", () => {
    let error: MetaTransactionError;

    beforeEach(() => {
      error = new MetaTransactionError("relay down", "UNKNOWN", 503, true, {});
    });

    it("should show the relay's own message", () => {
      expect(writeFailureText(error)).toBe("relay down");
    });
  });
});
