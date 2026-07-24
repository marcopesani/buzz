import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  ackReceipt,
  dropReceipt,
  emptyReceiptOutbox,
  enqueueReceipt,
  isReceiptPublished,
  markReceiptPublishError,
  pendingReceiptsToRetry,
  receiptDedupeKey,
} from "./receiptOutbox.ts";

const BASE = {
  requestEventId: "aa".repeat(32),
  paymentHash: "bb".repeat(32),
  preimage: "cc".repeat(32),
  amountMsat: 210_000,
  channelId: "11111111-1111-1111-1111-111111111111",
  nowUnix: 1_700_000_000,
};

describe("receiptOutbox", () => {
  it("enqueue → ack removes pending and records published key", () => {
    let state = emptyReceiptOutbox();
    state = enqueueReceipt(state, BASE);
    assert.equal(pendingReceiptsToRetry(state).length, 1);
    state = ackReceipt(state, BASE.requestEventId, BASE.paymentHash);
    assert.equal(pendingReceiptsToRetry(state).length, 0);
    assert.equal(
      isReceiptPublished(state, BASE.requestEventId, BASE.paymentHash),
      true,
    );
  });

  it("dedupes enqueue by (request_event_id, payment_hash)", () => {
    let state = emptyReceiptOutbox();
    state = enqueueReceipt(state, BASE);
    state = enqueueReceipt(state, { ...BASE, preimage: "dd".repeat(32) });
    assert.equal(state.pending.length, 1);
    assert.equal(state.pending[0].preimage, "cc".repeat(32));

    state = ackReceipt(state, BASE.requestEventId, BASE.paymentHash);
    state = enqueueReceipt(state, BASE);
    assert.equal(state.pending.length, 0);
  });

  it("drop removes from retry set but keeps the note", () => {
    let state = enqueueReceipt(emptyReceiptOutbox(), BASE);
    state = dropReceipt(
      state,
      BASE.requestEventId,
      BASE.paymentHash,
      "request_not_found",
    );
    assert.equal(pendingReceiptsToRetry(state).length, 0);
    assert.equal(state.pending[0].dropNote, "request_not_found");
  });

  it("enqueue revives a previously dropped entry", () => {
    let state = enqueueReceipt(emptyReceiptOutbox(), BASE);
    state = dropReceipt(
      state,
      BASE.requestEventId,
      BASE.paymentHash,
      "request_not_found",
    );
    state = enqueueReceipt(state, {
      ...BASE,
      channelId: "revived-channel",
      nowUnix: BASE.nowUnix + 10,
    });
    assert.equal(pendingReceiptsToRetry(state).length, 1);
    assert.equal(state.pending[0].dropNote, null);
    assert.equal(state.pending[0].channelId, "revived-channel");
  });

  it("markReceiptPublishError keeps entry retryable", () => {
    let state = enqueueReceipt(emptyReceiptOutbox(), BASE);
    state = markReceiptPublishError(
      state,
      BASE.requestEventId,
      BASE.paymentHash,
      "offline",
    );
    assert.equal(pendingReceiptsToRetry(state).length, 1);
    assert.equal(state.pending[0].lastError, "offline");
  });

  it("receiptDedupeKey normalizes case", () => {
    assert.equal(
      receiptDedupeKey("AA".repeat(32), "BB".repeat(32)),
      `${"aa".repeat(32)}:${"bb".repeat(32)}`,
    );
  });
});
