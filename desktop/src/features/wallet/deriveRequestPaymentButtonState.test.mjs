import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { deriveRequestPaymentButtonState } from "./deriveRequestPaymentButtonState.ts";

describe("deriveRequestPaymentButtonState", () => {
  it("asks the user to connect when the wallet is unlinked", () => {
    const state = deriveRequestPaymentButtonState({
      linked: false,
      capabilities: [],
    });
    assert.equal(state.kind, "connect_wallet");
    assert.equal(state.lookDisabled, true);
    assert.equal(state.tooltip, "Connect a wallet to request payment");
  });

  it("blocks invoice minting when make_invoice is missing", () => {
    const state = deriveRequestPaymentButtonState({
      linked: true,
      capabilities: ["pay_invoice", "get_balance"],
    });
    assert.equal(state.kind, "cannot_invoice");
    assert.equal(state.lookDisabled, true);
    assert.equal(state.tooltip, "Your wallet can't create invoices");
  });

  it("is ready when linked and make_invoice is present", () => {
    const state = deriveRequestPaymentButtonState({
      linked: true,
      capabilities: ["pay_invoice", "make_invoice"],
    });
    assert.equal(state.kind, "ready");
    assert.equal(state.lookDisabled, false);
    assert.equal(state.tooltip, "Request payment");
  });
});
