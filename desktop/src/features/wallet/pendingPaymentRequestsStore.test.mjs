import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";

import {
  emptyPendingPaymentRequests,
  trackPendingPaymentRequest,
} from "./pendingPaymentRequests.ts";
import {
  getPendingPaymentRequestsState,
  initPendingPaymentRequests,
  peekPendingPaymentRequestsForCommunity,
  resetPendingPaymentRequests,
  setPendingPaymentRequestsState,
  writePendingPaymentRequestsForCommunity,
} from "./pendingPaymentRequestsStore.ts";

function makeLocalStorage() {
  const map = new Map();
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => {
      map.set(k, String(v));
    },
    removeItem: (k) => {
      map.delete(k);
    },
    clear: () => map.clear(),
    key: (i) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  };
}

function installLocalStorage() {
  const ls = makeLocalStorage();
  globalThis.localStorage = ls;
  if (typeof globalThis.window === "undefined") {
    globalThis.window = { localStorage: ls };
  } else {
    globalThis.window.localStorage = ls;
  }
  ls.setItem("buzz-active-community-id", "community-a");
  return ls;
}

afterEach(() => {
  resetPendingPaymentRequests();
});

describe("pendingPaymentRequestsStore community isolation", () => {
  it("persists per community and does not leak across reset+init", () => {
    installLocalStorage();
    initPendingPaymentRequests("community-a");
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), {
        requestEventId: "aa".repeat(32),
        channelId: "ch-a",
        amountMsat: 1000,
        bolt11: "lnbc1a",
        paymentHash: "bb".repeat(32),
        createdAtUnix: 1,
        expiryUnix: 9_999_999_999,
      }),
    );
    assert.equal(getPendingPaymentRequestsState().pending.length, 1);

    resetPendingPaymentRequests();
    assert.equal(
      peekPendingPaymentRequestsForCommunity("community-a").pending.length,
      1,
    );

    initPendingPaymentRequests("community-b");
    assert.equal(getPendingPaymentRequestsState().pending.length, 0);
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), {
        requestEventId: "cc".repeat(32),
        channelId: "ch-b",
        amountMsat: 2000,
        bolt11: "lnbc1b",
        paymentHash: "dd".repeat(32),
        createdAtUnix: 2,
        expiryUnix: 9_999_999_999,
      }),
    );
    assert.equal(
      peekPendingPaymentRequestsForCommunity("community-a").pending[0]
        .channelId,
      "ch-a",
    );
    assert.equal(
      peekPendingPaymentRequestsForCommunity("community-b").pending[0]
        .channelId,
      "ch-b",
    );

    resetPendingPaymentRequests();
    initPendingPaymentRequests("community-a");
    assert.equal(getPendingPaymentRequestsState().pending[0].channelId, "ch-a");
  });

  it("survives process restart via localStorage", () => {
    installLocalStorage();
    writePendingPaymentRequestsForCommunity(
      "community-a",
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), {
        requestEventId: "ee".repeat(32),
        channelId: "ch-restart",
        amountMsat: 3000,
        bolt11: "lnbc1c",
        paymentHash: "ff".repeat(32),
        createdAtUnix: 3,
        expiryUnix: 9_999_999_999,
      }),
    );
    initPendingPaymentRequests("community-a");
    assert.equal(
      getPendingPaymentRequestsState().pending[0].channelId,
      "ch-restart",
    );
  });
});
