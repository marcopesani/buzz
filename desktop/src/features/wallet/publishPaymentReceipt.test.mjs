import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { emptyReceiptOutbox, enqueueReceipt } from "./receiptOutbox.ts";
import {
  flushReceiptOutbox,
  publishPaymentReceiptEntry,
} from "./publishPaymentReceipt.ts";

const REQUEST_ID = "aa".repeat(32);
const PAYMENT_HASH = "bb".repeat(32);
const PREIMAGE = "cc".repeat(32);
const CHANNEL = "11111111-1111-1111-1111-111111111111";

function seededState(channelId = CHANNEL) {
  return enqueueReceipt(emptyReceiptOutbox(), {
    requestEventId: REQUEST_ID,
    paymentHash: PAYMENT_HASH,
    preimage: PREIMAGE,
    amountMsat: 500_000,
    channelId,
    nowUnix: 1_700_000_000,
  });
}

describe("publishPaymentReceiptEntry", () => {
  it("publishes a 40010 with the schema tags", async () => {
    const published = [];
    const state = seededState();
    const entry = state.pending[0];
    const { state: next, result } = await publishPaymentReceiptEntry(
      state,
      entry,
      {
        fetchRequestEvent: async () => null,
        fetchReceiptsForRequest: async () => [],
        publishEvent: async (args) => {
          published.push(args);
        },
      },
    );
    assert.equal(result.kind, "published");
    assert.equal(next.pending.length, 0);
    assert.equal(published.length, 1);
    assert.equal(published[0].kind, 40010);
    assert.deepEqual(published[0].tags, [
      ["h", CHANNEL],
      ["e", REQUEST_ID],
      ["payment_hash", PAYMENT_HASH],
      ["preimage", PREIMAGE],
      ["amount", "500000"],
    ]);
  });

  it("acks without publishing when relay already has the receipt", async () => {
    const published = [];
    const state = seededState();
    const { result } = await publishPaymentReceiptEntry(
      state,
      state.pending[0],
      {
        fetchRequestEvent: async () => null,
        fetchReceiptsForRequest: async () => [
          {
            id: "ff".repeat(32),
            pubkey: "11".repeat(32),
            created_at: 1,
            kind: 40010,
            tags: [
              ["e", REQUEST_ID],
              ["payment_hash", PAYMENT_HASH],
              ["preimage", PREIMAGE],
              ["amount", "500000"],
            ],
            content: "",
            sig: "",
          },
        ],
        publishEvent: async (args) => {
          published.push(args);
        },
      },
    );
    assert.equal(result.kind, "already_on_relay");
    assert.equal(published.length, 0);
  });

  it("drops when the 40009 request cannot be resolved", async () => {
    const state = seededState(null);
    const { state: next, result } = await publishPaymentReceiptEntry(
      state,
      state.pending[0],
      {
        fetchRequestEvent: async () => null,
        fetchReceiptsForRequest: async () => [],
        publishEvent: async () => {
          throw new Error("should not publish");
        },
        log: () => {},
      },
    );
    assert.equal(result.kind, "dropped");
    assert.equal(result.note, "request_not_found");
    assert.equal(next.pending[0].dropNote, "request_not_found");
  });

  it("drops when resolved request id does not match the entry", async () => {
    const state = seededState(null);
    const { result } = await publishPaymentReceiptEntry(
      state,
      state.pending[0],
      {
        fetchRequestEvent: async () => ({
          id: "dd".repeat(32),
          pubkey: "11".repeat(32),
          created_at: 1,
          kind: 40009,
          tags: [
            ["h", CHANNEL],
            ["amount", "500000"],
          ],
          content: "",
          sig: "",
        }),
        fetchReceiptsForRequest: async () => [],
        publishEvent: async () => {
          throw new Error("should not publish");
        },
        log: () => {},
      },
    );
    assert.equal(result.kind, "dropped");
    assert.equal(result.note, "request_id_mismatch");
  });

  it("resolves channelId from the request event then publishes", async () => {
    const published = [];
    const state = seededState(null);
    const { result } = await publishPaymentReceiptEntry(
      state,
      state.pending[0],
      {
        fetchRequestEvent: async () => ({
          id: REQUEST_ID,
          pubkey: "11".repeat(32),
          created_at: 1,
          kind: 40009,
          tags: [
            ["h", CHANNEL],
            ["amount", "500000"],
            ["p", "11".repeat(32)],
            ["bolt11", "lnbc1"],
          ],
          content: "",
          sig: "",
        }),
        fetchReceiptsForRequest: async () => [],
        publishEvent: async (args) => {
          published.push(args);
        },
      },
    );
    assert.equal(result.kind, "published");
    assert.equal(published[0].tags.find((t) => t[0] === "h")?.[1], CHANNEL);
  });

  it("keeps entry pending on publish failure (retry)", async () => {
    const state = seededState();
    const { state: next, result } = await publishPaymentReceiptEntry(
      state,
      state.pending[0],
      {
        fetchRequestEvent: async () => null,
        fetchReceiptsForRequest: async () => [],
        publishEvent: async () => {
          throw new Error("mock publish rejected");
        },
      },
    );
    assert.equal(result.kind, "retry");
    assert.equal(next.pending.length, 1);
    assert.match(next.pending[0].lastError ?? "", /mock publish rejected/);
  });

  it("flush publishes exactly once across enqueue + relay-already-has", async () => {
    let publishes = 0;
    let state = seededState();
    state = await flushReceiptOutbox(state, {
      fetchRequestEvent: async () => null,
      fetchReceiptsForRequest: async () => [],
      publishEvent: async () => {
        publishes += 1;
      },
    });
    assert.equal(publishes, 1);

    // Re-enqueue same key → no-op; flush sees nothing pending.
    state = enqueueReceipt(state, {
      requestEventId: REQUEST_ID,
      paymentHash: PAYMENT_HASH,
      preimage: PREIMAGE,
      amountMsat: 500_000,
      channelId: CHANNEL,
      nowUnix: 1_700_000_001,
    });
    state = await flushReceiptOutbox(state, {
      fetchRequestEvent: async () => null,
      fetchReceiptsForRequest: async () => [],
      publishEvent: async () => {
        publishes += 1;
      },
    });
    assert.equal(publishes, 1);
  });
});
