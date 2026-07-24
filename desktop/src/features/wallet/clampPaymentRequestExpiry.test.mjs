import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { clampPaymentRequestExpiry } from "./clampPaymentRequestExpiry.ts";

describe("clampPaymentRequestExpiry", () => {
  it("uses invoice expiry when requested is null (CLI default)", () => {
    const result = clampPaymentRequestExpiry({
      requestedExpiryUnix: null,
      invoiceExpiresAtUnix: 1_700_000_000,
    });
    assert.equal(result.expiryUnix, 1_700_000_000);
    assert.equal(result.clamped, false);
  });

  it("keeps a requested expiry at the invoice bound", () => {
    const result = clampPaymentRequestExpiry({
      requestedExpiryUnix: 1_700_000_000,
      invoiceExpiresAtUnix: 1_700_000_000,
    });
    assert.equal(result.expiryUnix, 1_700_000_000);
    assert.equal(result.clamped, false);
  });

  it("keeps a requested expiry below the invoice", () => {
    const result = clampPaymentRequestExpiry({
      requestedExpiryUnix: 1_699_000_000,
      invoiceExpiresAtUnix: 1_700_000_000,
    });
    assert.equal(result.expiryUnix, 1_699_000_000);
    assert.equal(result.clamped, false);
  });

  it("clamps a requested expiry above the invoice", () => {
    const result = clampPaymentRequestExpiry({
      requestedExpiryUnix: 1_800_000_000,
      invoiceExpiresAtUnix: 1_700_000_000,
    });
    assert.equal(result.expiryUnix, 1_700_000_000);
    assert.equal(result.clamped, true);
  });
});
