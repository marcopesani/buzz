/**
 * Wallet reconcile + receipt outbox flush — single entry for Path A enqueue
 * and Path B reconcile. ConfirmSendDialog and useWalletStatus call here;
 * they never publish receipts ad hoc.
 */

import { relayClient } from "@/shared/api/relayClient";
import { signRelayEvent } from "@/shared/api/tauri";
import type { RelayEvent } from "@/shared/api/types";

import { enqueueReceipt, type EnqueueReceiptInput } from "./receiptOutbox";
import {
  getReceiptOutboxState,
  setReceiptOutboxState,
} from "./receiptOutboxStore";
import {
  flushReceiptOutbox,
  paymentReceiptsByRequestFilter,
  paymentRequestByIdFilter,
  type PublishPaymentReceiptDeps,
} from "./publishPaymentReceipt";
import { walletReconcile, type SettledPayRequest } from "./walletApi";

/** Serialize enqueue/flush so confirm + reconcile never interleave writes. */
let flushChain: Promise<void> = Promise.resolve();

function livePublishDeps(): PublishPaymentReceiptDeps {
  return {
    fetchRequestEvent: async (requestEventId) => {
      const events = await relayClient.fetchEvents(
        paymentRequestByIdFilter(requestEventId),
      );
      return events[0] ?? null;
    },
    fetchReceiptsForRequest: async (requestEventId) => {
      return relayClient.fetchEvents(
        paymentReceiptsByRequestFilter(requestEventId),
      );
    },
    publishEvent: async ({ kind, content, tags }) => {
      const event = await signRelayEvent({ kind, content, tags });
      await relayClient.publishEvent(
        event as RelayEvent,
        "Timed out while publishing the payment receipt.",
        "Failed to publish the payment receipt.",
      );
    },
  };
}

function withFlushLock(run: () => Promise<void>): Promise<void> {
  const next = flushChain.then(run, run);
  flushChain = next.then(
    () => undefined,
    () => undefined,
  );
  return next;
}

/**
 * Enqueue a settled pay-request receipt and attempt publish immediately.
 * Used by ConfirmSendDialog Path A when confirm returns settled+preimage.
 */
export async function enqueueAndPublishReceipt(input: {
  requestEventId: string;
  paymentHash: string;
  preimage: string;
  amountMsat: number;
  channelId: string | null;
}): Promise<void> {
  installE2eReconcileHook();
  await withFlushLock(async () => {
    const nowUnix = Math.floor(Date.now() / 1000);
    const enqueueInput: EnqueueReceiptInput = {
      requestEventId: input.requestEventId,
      paymentHash: input.paymentHash,
      preimage: input.preimage,
      amountMsat: input.amountMsat,
      channelId: input.channelId,
      nowUnix,
    };
    let state = enqueueReceipt(getReceiptOutboxState(), enqueueInput);
    setReceiptOutboxState(state);
    state = await flushReceiptOutbox(state, livePublishDeps());
    setReceiptOutboxState(state);
  });
}

function enqueueSettledPayRequests(
  settled: SettledPayRequest[],
  nowUnix: number,
): void {
  let state = getReceiptOutboxState();
  for (const row of settled) {
    if (!row.preimage) {
      console.warn(
        `[wallet-receipt] skipping settled pay_request without preimage: ${row.request_event_id}`,
      );
      continue;
    }
    state = enqueueReceipt(state, {
      requestEventId: row.request_event_id,
      paymentHash: row.payment_hash,
      preimage: row.preimage,
      amountMsat: row.amount_msat,
      channelId: null,
      nowUnix,
    });
  }
  setReceiptOutboxState(state);
}

/**
 * Path B: call `walletReconcile()`, enqueue newly settled pay_requests, then
 * flush the durable outbox (retries + new settles). Piggybacks existing
 * reconcile call sites — no dedicated timer.
 */
export async function reconcileAndFlushReceipts(): Promise<
  SettledPayRequest[]
> {
  installE2eReconcileHook();
  let settled: SettledPayRequest[] = [];
  try {
    settled = await walletReconcile();
  } catch (err) {
    console.warn(
      "[wallet-receipt] walletReconcile failed:",
      err instanceof Error ? err.message : String(err),
    );
  }

  await withFlushLock(async () => {
    const nowUnix = Math.floor(Date.now() / 1000);
    enqueueSettledPayRequests(settled, nowUnix);
    const next = await flushReceiptOutbox(
      getReceiptOutboxState(),
      livePublishDeps(),
    );
    setReceiptOutboxState(next);
  });

  return settled;
}

function installE2eReconcileHook(): void {
  if (typeof window === "undefined") return;
  const e2eWindow = window as Window & {
    __BUZZ_E2E__?: unknown;
    __BUZZ_E2E_RECONCILE_RECEIPTS__?: () => Promise<SettledPayRequest[]>;
  };
  if (e2eWindow.__BUZZ_E2E__) {
    e2eWindow.__BUZZ_E2E_RECONCILE_RECEIPTS__ = reconcileAndFlushReceipts;
  }
}

// E2E: allow specs to trigger a reconcile+flush tick without new UI chrome.
installE2eReconcileHook();
