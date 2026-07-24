import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { buildPaymentReceiptEvent } from "./buildPaymentReceiptEvent.ts";
import { derivePayCardState } from "./derivePayCardState.ts";
import { parsePaymentReceipt } from "./parsePaymentReceipt.ts";

describe("buildPaymentReceiptEvent", () => {
  it("matches the SDK/CLI 40010 tag schema and drives paid card state", () => {
    const channelId = "11111111-1111-1111-1111-111111111111";
    const requestEventId = "aa".repeat(32);
    const paymentHash = "bb".repeat(32);
    const preimage = "cc".repeat(32);
    const built = buildPaymentReceiptEvent({
      channelId,
      requestEventId,
      paymentHash,
      preimage,
      amountMsat: 500_000,
    });

    assert.equal(built.kind, 40010);
    assert.equal(built.content, "");
    assert.deepEqual(built.tags, [
      ["h", channelId],
      ["e", requestEventId],
      ["payment_hash", paymentHash],
      ["preimage", preimage],
      ["amount", "500000"],
    ]);
    assert.equal(
      built.tags.some((t) => t[0] === "p"),
      false,
      "receipts must not carry a p tag",
    );

    const parsed = parsePaymentReceipt(built.tags);
    assert.equal(parsed.requestEventId, requestEventId);
    assert.equal(parsed.paymentHash, paymentHash);
    assert.equal(parsed.preimage, preimage);
    assert.equal(parsed.amountMsat, 500_000);
    assert.equal(parsed.channelId, channelId);

    const card = derivePayCardState({
      requestPubkey: "dd".repeat(32),
      myPubkey: "ee".repeat(32),
      expiryUnix: null,
      nowUnix: 1_700_000_000,
      hasReceipt: Boolean(parsed.requestEventId && parsed.preimage),
      walletLinked: true,
    });
    assert.equal(card.kind, "paid");
  });

  it("rejects invalid amount and non-hex ids", () => {
    assert.throws(() =>
      buildPaymentReceiptEvent({
        channelId: "c",
        requestEventId: "aa".repeat(32),
        paymentHash: "bb".repeat(32),
        preimage: "cc".repeat(32),
        amountMsat: 0,
      }),
    );
    assert.throws(() =>
      buildPaymentReceiptEvent({
        channelId: "c",
        requestEventId: "not-hex",
        paymentHash: "bb".repeat(32),
        preimage: "cc".repeat(32),
        amountMsat: 1,
      }),
    );
  });
});
