import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  applyIncomingCheck,
  dropExpiredPendingPaymentRequests,
  emptyPendingPaymentRequests,
  findPendingPaymentRequest,
  isVerifiedPaymentRequest,
  trackPendingPaymentRequest,
} from "./pendingPaymentRequests.ts";

const BASE = {
  requestEventId: "aa".repeat(32),
  channelId: "ch-1",
  amountMsat: 500_000,
  bolt11: "lnbc5u1pmock",
  paymentHash: "bb".repeat(32),
  createdAtUnix: 1_700_000_000,
  expiryUnix: 1_700_003_600,
};

describe("pendingPaymentRequests registry", () => {
  it("adds on publish and finds by id", () => {
    const state = trackPendingPaymentRequest(
      emptyPendingPaymentRequests(),
      BASE,
    );
    assert.equal(state.pending.length, 1);
    assert.equal(
      findPendingPaymentRequest(state, BASE.requestEventId)?.amountMsat,
      500_000,
    );
  });

  it("verified-paid removes from pending and records verifiedIds", () => {
    let state = trackPendingPaymentRequest(emptyPendingPaymentRequests(), BASE);
    const result = applyIncomingCheck(state, {
      requestEventId: BASE.requestEventId,
      status: "paid",
      nowUnix: 1_700_000_100,
    });
    assert.equal(result.kind, "verified");
    state = result.state;
    assert.equal(state.pending.length, 0);
    assert.equal(isVerifiedPaymentRequest(state, BASE.requestEventId), true);
  });

  it("dedupes double verification — second paid is already_verified", () => {
    const state = trackPendingPaymentRequest(
      emptyPendingPaymentRequests(),
      BASE,
    );
    const first = applyIncomingCheck(state, {
      requestEventId: BASE.requestEventId,
      status: "paid",
      nowUnix: 1_700_000_100,
    });
    assert.equal(first.kind, "verified");
    const second = applyIncomingCheck(first.state, {
      requestEventId: BASE.requestEventId,
      status: "paid",
      nowUnix: 1_700_000_200,
    });
    assert.equal(second.kind, "already_verified");
    assert.equal(second.state.verifiedIds.length, 1);
  });

  it("unpaid keeps pending", () => {
    const state = trackPendingPaymentRequest(
      emptyPendingPaymentRequests(),
      BASE,
    );
    const result = applyIncomingCheck(state, {
      requestEventId: BASE.requestEventId,
      status: "unpaid",
      nowUnix: 1_700_000_100,
    });
    assert.equal(result.kind, "still_pending");
    assert.equal(result.state.pending.length, 1);
  });

  it("unconfirmable keeps pending and stamps lastUnconfirmableAtUnix", () => {
    const state = trackPendingPaymentRequest(
      emptyPendingPaymentRequests(),
      BASE,
    );
    const result = applyIncomingCheck(state, {
      requestEventId: BASE.requestEventId,
      status: "unconfirmable",
      nowUnix: 1_700_000_111,
    });
    assert.equal(result.kind, "unconfirmable");
    assert.equal(result.state.pending.length, 1);
    assert.equal(
      result.state.pending[0].lastUnconfirmableAtUnix,
      1_700_000_111,
    );
  });

  it("drops expired silently from pending", () => {
    const state = trackPendingPaymentRequest(
      emptyPendingPaymentRequests(),
      BASE,
    );
    const { state: next, expiredIds } = dropExpiredPendingPaymentRequests(
      state,
      BASE.expiryUnix,
    );
    assert.deepEqual(expiredIds, [BASE.requestEventId.toLowerCase()]);
    assert.equal(next.pending.length, 0);
  });

  it("track after verified is a no-op", () => {
    let state = trackPendingPaymentRequest(emptyPendingPaymentRequests(), BASE);
    const paid = applyIncomingCheck(state, {
      requestEventId: BASE.requestEventId,
      status: "paid",
      nowUnix: 1_700_000_100,
    });
    state = trackPendingPaymentRequest(paid.state, BASE);
    assert.equal(state.pending.length, 0);
    assert.equal(state.verifiedIds.length, 1);
  });
});
