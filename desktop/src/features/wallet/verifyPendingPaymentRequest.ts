/**
 * Single verify path for MY published payment requests.
 *
 * Receipt trigger and poll cadence both call here. A 40010 never flips
 * verification — only walletCheckIncoming does.
 *
 * Unconfirmable policy: keep pending, stamp lastUnconfirmableAtUnix, rely on
 * the existing reconcile cadence (no new retry timer).
 */

import { KIND_PAYMENT_RECEIPT } from "@/shared/constants/kinds";

import { getReceiptRequestId } from "./parsePaymentRequest";
import { emitPaymentEvent } from "./paymentEvents";
import {
  applyIncomingCheck,
  dropExpiredPendingPaymentRequests,
  findPendingPaymentRequest,
  isVerifiedPaymentRequest,
  trackPendingPaymentRequest,
  type TrackPendingPaymentRequestInput,
} from "./pendingPaymentRequests";
import {
  getPendingPaymentRequestsState,
  setPendingPaymentRequestsState,
} from "./pendingPaymentRequestsStore";
import { walletCheckIncoming, type IncomingCheckOutcome } from "./walletApi";
import { isWalletExperimentEnabled } from "./walletFeatureEnabled";

export type VerifyPendingDeps = {
  checkIncoming: (input: {
    bolt11?: string | null;
    lud16?: string | null;
  }) => Promise<IncomingCheckOutcome>;
  nowUnix?: () => number;
  /** Inject for tests; defaults to the live wallet experiment check. */
  isWalletEnabled?: () => boolean;
};

const liveDeps = (): VerifyPendingDeps => ({
  checkIncoming: walletCheckIncoming,
  nowUnix: () => Math.floor(Date.now() / 1000),
  isWalletEnabled: isWalletExperimentEnabled,
});

function walletEnabled(deps: VerifyPendingDeps): boolean {
  return (deps.isWalletEnabled ?? isWalletExperimentEnabled)();
}

/** Serialize verifies so receipt + poll never interleave the same entry. */
let verifyChain: Promise<void> = Promise.resolve();

function withVerifyLock(run: () => Promise<void>): Promise<void> {
  const next = verifyChain.then(run, run);
  verifyChain = next.then(
    () => undefined,
    () => undefined,
  );
  return next;
}

function emitExpiredFromDrop(
  expiredIds: string[],
  priorPending: Array<{
    requestEventId: string;
    channelId: string;
    amountMsat: number;
  }>,
): void {
  for (const expiredId of expiredIds) {
    const prior = priorPending.find((row) => row.requestEventId === expiredId);
    emitPaymentEvent({
      type: "request_expired",
      requestEventId: expiredId,
      channelId: prior?.channelId ?? "",
      amountMsat: prior?.amountMsat ?? 0,
    });
  }
}

/** Register a freshly published 40009 into the durable pending registry. */
export function registerPublishedPaymentRequest(
  input: TrackPendingPaymentRequestInput,
): void {
  const next = trackPendingPaymentRequest(
    getPendingPaymentRequestsState(),
    input,
  );
  setPendingPaymentRequestsState(next);
}

export type VerifyTrigger = "receipt" | "poll";

/**
 * Run walletCheckIncoming for one pending request and apply the outcome.
 * No-op when not pending or already verified.
 */
export async function verifyPendingPaymentRequest(
  requestEventId: string,
  _trigger: VerifyTrigger,
  deps: VerifyPendingDeps = liveDeps(),
): Promise<void> {
  await withVerifyLock(async () => {
    const nowUnix = deps.nowUnix?.() ?? Math.floor(Date.now() / 1000);
    let state = getPendingPaymentRequestsState();
    const priorPending = state.pending;

    const dropped = dropExpiredPendingPaymentRequests(state, nowUnix);
    if (dropped.expiredIds.length > 0) {
      setPendingPaymentRequestsState(dropped.state);
      emitExpiredFromDrop(dropped.expiredIds, priorPending);
      state = dropped.state;
    }

    if (isVerifiedPaymentRequest(state, requestEventId)) {
      return;
    }

    const entry = findPendingPaymentRequest(state, requestEventId);
    if (!entry?.bolt11) {
      return;
    }

    let status: IncomingCheckOutcome["status"];
    try {
      const outcome = await deps.checkIncoming({ bolt11: entry.bolt11 });
      status = outcome.status;
    } catch (err) {
      console.warn(
        "[wallet-payment-events] walletCheckIncoming failed:",
        err instanceof Error ? err.message : String(err),
      );
      status = "unconfirmable";
    }

    // Re-read after await — community may have switched.
    state = getPendingPaymentRequestsState();
    const result = applyIncomingCheck(state, {
      requestEventId,
      status,
      nowUnix: deps.nowUnix?.() ?? Math.floor(Date.now() / 1000),
    });

    switch (result.kind) {
      case "verified":
        setPendingPaymentRequestsState(result.state);
        emitPaymentEvent({
          type: "request_paid_verified",
          requestEventId: result.entry.requestEventId,
          channelId: result.entry.channelId,
          amountMsat: result.entry.amountMsat,
        });
        return;
      case "still_pending":
        return;
      case "unconfirmable":
        setPendingPaymentRequestsState(result.state);
        return;
      case "expired":
        setPendingPaymentRequestsState(result.state);
        emitPaymentEvent({
          type: "request_expired",
          requestEventId: result.requestEventId,
          channelId: entry.channelId,
          amountMsat: entry.amountMsat,
        });
        return;
      case "already_verified":
      case "not_pending":
        return;
      default: {
        const _exhaustive: never = result;
        return _exhaustive;
      }
    }
  });
}

/**
 * Poll backstop: verify every pending entry. Called from the existing
 * reconcile/flush cadence — no dedicated timer.
 */
export async function verifyAllPendingPaymentRequests(
  deps: VerifyPendingDeps = liveDeps(),
): Promise<void> {
  if (!walletEnabled(deps)) return;

  const nowUnix = deps.nowUnix?.() ?? Math.floor(Date.now() / 1000);
  let state = getPendingPaymentRequestsState();
  const priorPending = state.pending;
  const dropped = dropExpiredPendingPaymentRequests(state, nowUnix);
  if (dropped.expiredIds.length > 0) {
    setPendingPaymentRequestsState(dropped.state);
    emitExpiredFromDrop(dropped.expiredIds, priorPending);
    state = dropped.state;
  }

  const ids = state.pending.map((entry) => entry.requestEventId);
  for (const id of ids) {
    await verifyPendingPaymentRequest(id, "poll", deps);
  }
}

/**
 * Receipt trigger: when a live 40010 arrives, verify the referenced request
 * if it is in our pending registry. The receipt itself never flips state.
 */
export function observePaymentReceiptForVerification(
  event: {
    kind: number;
    tags: string[][];
  },
  deps: VerifyPendingDeps = liveDeps(),
): void {
  if (!walletEnabled(deps)) return;
  if (event.kind !== KIND_PAYMENT_RECEIPT) return;
  const requestEventId = getReceiptRequestId(event.tags);
  if (!requestEventId) return;
  const state = getPendingPaymentRequestsState();
  if (!findPendingPaymentRequest(state, requestEventId)) return;
  if (isVerifiedPaymentRequest(state, requestEventId)) return;
  void verifyPendingPaymentRequest(requestEventId, "receipt", deps);
}
