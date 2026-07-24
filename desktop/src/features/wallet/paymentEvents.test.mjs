import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";

import {
  emitPaymentEvent,
  resetPaymentEvents,
  subscribePaymentEvents,
} from "./paymentEvents.ts";

afterEach(() => {
  resetPaymentEvents();
});

describe("paymentEvents signal", () => {
  it("emits typed events to subscribers", () => {
    const seen = [];
    const unsub = subscribePaymentEvents((event) => {
      seen.push(event);
    });
    emitPaymentEvent({
      type: "request_paid_verified",
      requestEventId: "aa".repeat(32),
      channelId: "ch-1",
      amountMsat: 210_000,
    });
    assert.equal(seen.length, 1);
    assert.equal(seen[0].type, "request_paid_verified");
    assert.equal(seen[0].amountMsat, 210_000);
    unsub();
  });

  it("unsubscribe stops delivery", () => {
    const seen = [];
    const unsub = subscribePaymentEvents((event) => {
      seen.push(event);
    });
    unsub();
    emitPaymentEvent({
      type: "request_expired",
      requestEventId: "bb".repeat(32),
      channelId: "ch-2",
      amountMsat: 1000,
    });
    assert.equal(seen.length, 0);
  });

  it("reset clears all listeners", () => {
    const seen = [];
    subscribePaymentEvents((event) => {
      seen.push(event);
    });
    resetPaymentEvents();
    emitPaymentEvent({
      type: "request_paid_verified",
      requestEventId: "cc".repeat(32),
      channelId: "ch-3",
      amountMsat: 3000,
    });
    assert.equal(seen.length, 0);
  });
});
