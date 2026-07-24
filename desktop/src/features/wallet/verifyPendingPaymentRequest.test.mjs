import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";

import { resetPaymentEvents, subscribePaymentEvents } from "./paymentEvents.ts";
import {
  emptyPendingPaymentRequests,
  trackPendingPaymentRequest,
} from "./pendingPaymentRequests.ts";
import {
  getPendingPaymentRequestsState,
  initPendingPaymentRequests,
  resetPendingPaymentRequests,
  setPendingPaymentRequestsState,
} from "./pendingPaymentRequestsStore.ts";
import {
  ensurePaymentEventsToast,
  resetPaymentEventsToast,
} from "./paymentEventsToast.ts";
import {
  observePaymentReceiptForVerification,
  verifyAllPendingPaymentRequests,
  verifyPendingPaymentRequest,
} from "./verifyPendingPaymentRequest.ts";

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

const REQUEST_ID = "11".repeat(32);
const ENTRY = {
  requestEventId: REQUEST_ID,
  channelId: "ch-1",
  amountMsat: 210_000,
  bolt11: "lnbc210n1pmock",
  paymentHash: "22".repeat(32),
  createdAtUnix: 1_700_000_000,
  expiryUnix: 9_999_999_999,
};

afterEach(() => {
  resetPendingPaymentRequests();
  resetPaymentEventsToast();
  resetPaymentEvents();
});

describe("verifyPendingPaymentRequest state machine", () => {
  it("receipt and poll funnel to one check; unpaid keeps pending; paid emits once", async () => {
    installLocalStorage();
    initPendingPaymentRequests("community-a");
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), ENTRY),
    );

    const outcomes = [{ status: "unpaid" }, { status: "paid" }];
    let calls = 0;
    const deps = {
      checkIncoming: async () => {
        const next = outcomes[calls] ?? { status: "unpaid" };
        calls += 1;
        return next;
      },
      nowUnix: () => 1_700_000_100,
      isWalletEnabled: () => true,
    };

    const events = [];
    subscribePaymentEvents((event) => {
      events.push(event);
    });

    observePaymentReceiptForVerification(
      {
        kind: 40010,
        tags: [
          ["e", REQUEST_ID],
          ["amount", "210000"],
        ],
      },
      deps,
    );
    await new Promise((r) => setTimeout(r, 20));
    assert.equal(calls, 1);
    assert.equal(events.length, 0);
    assert.equal(getPendingPaymentRequestsState().pending.length, 1);

    await verifyPendingPaymentRequest(REQUEST_ID, "poll", deps);
    assert.equal(calls, 2);
    assert.equal(events.length, 1);
    assert.equal(events[0].type, "request_paid_verified");
    assert.equal(events[0].requestEventId, REQUEST_ID);
    assert.equal(getPendingPaymentRequestsState().pending.length, 0);
    assert.equal(getPendingPaymentRequestsState().verifiedIds.length, 1);

    await verifyPendingPaymentRequest(REQUEST_ID, "receipt", deps);
    assert.equal(events.length, 1);
    assert.equal(calls, 2);
  });

  it("unconfirmable keeps pending for cadence retry", async () => {
    installLocalStorage();
    initPendingPaymentRequests("community-a");
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), ENTRY),
    );

    await verifyPendingPaymentRequest(REQUEST_ID, "poll", {
      checkIncoming: async () => ({ status: "unconfirmable" }),
      nowUnix: () => 1_700_000_222,
      isWalletEnabled: () => true,
    });

    const state = getPendingPaymentRequestsState();
    assert.equal(state.pending.length, 1);
    assert.equal(state.pending[0].lastUnconfirmableAtUnix, 1_700_000_222);
  });

  it("duplicate receipt injections still emit one verified event", async () => {
    installLocalStorage();
    initPendingPaymentRequests("community-a");
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), ENTRY),
    );

    const events = [];
    subscribePaymentEvents((event) => {
      events.push(event);
    });

    const deps = {
      checkIncoming: async () => ({ status: "paid" }),
      nowUnix: () => 1_700_000_100,
      isWalletEnabled: () => true,
    };
    const receipt = {
      kind: 40010,
      tags: [["e", REQUEST_ID]],
    };
    observePaymentReceiptForVerification(receipt, deps);
    observePaymentReceiptForVerification(receipt, deps);
    await new Promise((r) => setTimeout(r, 40));
    assert.equal(events.length, 1);
    assert.equal(events[0].type, "request_paid_verified");
  });

  it("experiment off + leftover pending + receipt → zero checkIncoming calls", async () => {
    installLocalStorage();
    initPendingPaymentRequests("community-a");
    setPendingPaymentRequestsState(
      trackPendingPaymentRequest(emptyPendingPaymentRequests(), ENTRY),
    );

    let calls = 0;
    const deps = {
      checkIncoming: async () => {
        calls += 1;
        return { status: "paid" };
      },
      nowUnix: () => 1_700_000_100,
      isWalletEnabled: () => false,
    };

    const events = [];
    subscribePaymentEvents((event) => {
      events.push(event);
    });

    // Toast install must be a no-op when the experiment is off.
    assert.equal(
      ensurePaymentEventsToast(() => false),
      false,
    );

    observePaymentReceiptForVerification(
      {
        kind: 40010,
        tags: [["e", REQUEST_ID]],
      },
      deps,
    );
    await verifyAllPendingPaymentRequests(deps);
    await new Promise((r) => setTimeout(r, 20));

    assert.equal(calls, 0);
    assert.equal(events.length, 0);
    assert.equal(getPendingPaymentRequestsState().pending.length, 1);
  });
});
