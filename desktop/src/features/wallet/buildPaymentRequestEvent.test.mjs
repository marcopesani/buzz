import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { buildPaymentRequestEvent } from "./buildPaymentRequestEvent.ts";
import { derivePayCardState } from "./derivePayCardState.ts";
import { parsePaymentRequest } from "./parsePaymentRequest.ts";

describe("buildPaymentRequestEvent", () => {
  it("matches the CLI/SDK 40009 tag schema parseable by the pay card", () => {
    const channelId = "11111111-1111-1111-1111-111111111111";
    const payee = "aa".repeat(32);
    const bolt11 = "lnbc500u1pexampleinvoice";
    const expiry = 1_700_000_000;
    const built = buildPaymentRequestEvent({
      channelId,
      amountMsat: 500_000,
      payeePubkey: payee,
      bolt11,
      memo: "lunch",
      expiryUnix: expiry,
    });

    assert.equal(built.kind, 40009);
    assert.equal(built.content, "");
    assert.deepEqual(built.tags, [
      ["h", channelId],
      ["amount", "500000"],
      ["p", payee],
      ["bolt11", bolt11],
      ["memo", "lunch"],
      ["expiry", String(expiry)],
    ]);

    const parsed = parsePaymentRequest(built.tags);
    assert.equal(parsed.amountMsat, 500_000);
    assert.equal(parsed.memo, "lunch");
    assert.equal(parsed.bolt11, bolt11);
    assert.equal(parsed.expiryUnix, expiry);
    assert.equal(parsed.payeePubkey, payee);

    // Round-trip: stranger + linked → payable with the built amount.
    const card = derivePayCardState({
      requestPubkey: payee,
      myPubkey: "bb".repeat(32),
      expiryUnix: parsed.expiryUnix,
      nowUnix: expiry - 60,
      hasReceipt: false,
      walletLinked: true,
    });
    assert.equal(card.kind, "payable");
  });

  it("omits empty memo and null expiry", () => {
    const built = buildPaymentRequestEvent({
      channelId: "11111111-1111-1111-1111-111111111111",
      amountMsat: 1000,
      payeePubkey: "aa".repeat(32),
      bolt11: "lnbc1",
      memo: "  ",
      expiryUnix: null,
    });
    assert.equal(
      built.tags.some((t) => t[0] === "memo"),
      false,
    );
    assert.equal(
      built.tags.some((t) => t[0] === "expiry"),
      false,
    );
    assert.equal(
      built.tags.some((t) => t[0] === "expiration"),
      false,
    );
  });
});
