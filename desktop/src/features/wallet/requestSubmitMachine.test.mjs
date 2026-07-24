import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  checkChannelEpoch,
  createSubmitLatch,
  reduceRequestSubmit,
} from "./requestSubmitMachine.ts";
import { executeRequestPaymentSubmit } from "./executeRequestPaymentSubmit.ts";

const INVOICE = {
  bolt11: "lnbc210n1sticky",
  payment_hash: "ab".repeat(32),
  expires_at_unix: 1_700_003_600,
};

describe("reduceRequestSubmit", () => {
  it("moves form → busy → retryable → busy on retry", () => {
    let state = { kind: "form" };
    state = reduceRequestSubmit(state, { type: "begin" });
    assert.equal(state.kind, "busy");
    state = reduceRequestSubmit(state, {
      type: "publish_failed",
      invoice: INVOICE,
      amountMsat: 210_000,
      memo: "x",
      expiryUnix: INVOICE.expires_at_unix,
      channelId: "ch-1",
      error: "relay down",
    });
    assert.equal(state.kind, "retryable");
    assert.equal(state.invoice.bolt11, INVOICE.bolt11);
    state = reduceRequestSubmit(state, { type: "begin_retry" });
    assert.equal(state.kind, "busy");
  });
});

describe("createSubmitLatch", () => {
  it("single-flight: second enter fails until exit", () => {
    const latch = createSubmitLatch();
    assert.equal(latch.tryEnter(), true);
    assert.equal(latch.tryEnter(), false);
    assert.equal(latch.isLocked(), true);
    latch.exit();
    assert.equal(latch.tryEnter(), true);
  });
});

describe("checkChannelEpoch", () => {
  it("aborts when publish target differs from opened channel", () => {
    const result = checkChannelEpoch({
      openedChannelId: "ch-a",
      currentChannelId: "ch-a",
      publishChannelId: "ch-b",
    });
    assert.equal(result.ok, false);
  });

  it("aborts when live channel navigates away from opened", () => {
    const result = checkChannelEpoch({
      openedChannelId: "ch-a",
      currentChannelId: "ch-b",
      publishChannelId: "ch-a",
    });
    assert.equal(result.ok, false);
  });

  it("allows DM prepare when opened channel was null", () => {
    const result = checkChannelEpoch({
      openedChannelId: null,
      currentChannelId: null,
      publishChannelId: "ch-new",
    });
    assert.equal(result.ok, true);
  });
});

describe("executeRequestPaymentSubmit", () => {
  it("prepares the DM channel before minting", async () => {
    const order = [];
    const result = await executeRequestPaymentSubmit({
      mode: "mint_and_publish",
      amountMsat: 210_000,
      memo: "coffee",
      requestedExpiryUnix: null,
      openedChannelId: null,
      getCurrentChannelId: () => null,
      payeePubkey: "aa".repeat(32),
      existingInvoice: null,
      existingChannelId: null,
      prepareChannel: async () => {
        order.push("prepare");
        return "ch-dm";
      },
      mintInvoice: async () => {
        order.push("mint");
        return INVOICE;
      },
      publishEvent: async () => {
        order.push("publish");
      },
    });
    assert.equal(result.ok, true);
    assert.deepEqual(order, ["prepare", "mint", "publish"]);
  });

  it("retry reuses the same bolt11 without a second mint", async () => {
    let mintCalls = 0;
    const published = [];
    const first = await executeRequestPaymentSubmit({
      mode: "mint_and_publish",
      amountMsat: 210_000,
      memo: null,
      requestedExpiryUnix: null,
      openedChannelId: "ch-1",
      getCurrentChannelId: () => "ch-1",
      payeePubkey: "aa".repeat(32),
      existingInvoice: null,
      existingChannelId: null,
      mintInvoice: async () => {
        mintCalls += 1;
        return INVOICE;
      },
      publishEvent: async () => {
        throw new Error("relay disconnect");
      },
    });
    assert.equal(first.ok, false);
    assert.equal(first.retryable, true);
    assert.equal(first.invoice?.bolt11, INVOICE.bolt11);
    assert.equal(mintCalls, 1);

    const second = await executeRequestPaymentSubmit({
      mode: "retry_publish",
      amountMsat: 210_000,
      memo: null,
      requestedExpiryUnix: first.expiryUnix,
      openedChannelId: "ch-1",
      getCurrentChannelId: () => "ch-1",
      payeePubkey: "aa".repeat(32),
      existingInvoice: first.invoice,
      existingChannelId: first.channelId,
      mintInvoice: async () => {
        mintCalls += 1;
        return { ...INVOICE, bolt11: "lnbc-should-not-mint" };
      },
      publishEvent: async ({ tags }) => {
        published.push(tags.find((t) => t[0] === "bolt11")?.[1]);
      },
    });
    assert.equal(second.ok, true);
    assert.equal(mintCalls, 1);
    assert.deepEqual(published, [INVOICE.bolt11]);
  });

  it("aborts on epoch mismatch without publishing", async () => {
    let published = false;
    const result = await executeRequestPaymentSubmit({
      mode: "mint_and_publish",
      amountMsat: 210_000,
      memo: null,
      requestedExpiryUnix: null,
      openedChannelId: "ch-old",
      getCurrentChannelId: () => "ch-new",
      payeePubkey: "aa".repeat(32),
      existingInvoice: null,
      existingChannelId: null,
      mintInvoice: async () => INVOICE,
      publishEvent: async () => {
        published = true;
      },
    });
    assert.equal(result.ok, false);
    assert.match(result.error, /Channel changed/);
    assert.equal(published, false);
  });

  it("clamps event expiry above the invoice", async () => {
    const result = await executeRequestPaymentSubmit({
      mode: "mint_and_publish",
      amountMsat: 21_000,
      memo: null,
      requestedExpiryUnix: INVOICE.expires_at_unix + 86_400,
      openedChannelId: "ch-1",
      getCurrentChannelId: () => "ch-1",
      payeePubkey: "aa".repeat(32),
      existingInvoice: null,
      existingChannelId: null,
      mintInvoice: async () => INVOICE,
      publishEvent: async () => {},
    });
    assert.equal(result.ok, true);
    assert.equal(result.clamped, true);
    assert.equal(result.expiryUnix, INVOICE.expires_at_unix);
    assert.equal(
      result.tags.find((t) => t[0] === "expiry")?.[1],
      String(INVOICE.expires_at_unix),
    );
  });
});
