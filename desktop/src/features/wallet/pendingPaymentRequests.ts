/**
 * Pure pending-payment-request registry transitions.
 *
 * One record owns a published 40009 awaiting wallet-verified settlement.
 * Receipts never flip state here — only walletCheckIncoming outcomes do.
 */

export type PendingPaymentRequestEntry = {
  requestEventId: string;
  channelId: string;
  amountMsat: number;
  bolt11: string;
  paymentHash: string;
  createdAtUnix: number;
  expiryUnix: number;
  /** Last time check_incoming returned unconfirmable (null if never). */
  lastUnconfirmableAtUnix: number | null;
};

export type PendingPaymentRequestsState = {
  pending: PendingPaymentRequestEntry[];
  /** Already wallet-verified paid — never toast/signal again. */
  verifiedIds: string[];
};

export function emptyPendingPaymentRequests(): PendingPaymentRequestsState {
  return { pending: [], verifiedIds: [] };
}

export type TrackPendingPaymentRequestInput = {
  requestEventId: string;
  channelId: string;
  amountMsat: number;
  bolt11: string;
  paymentHash: string;
  createdAtUnix: number;
  expiryUnix: number;
};

/** Add (or refresh) a pending entry after our 40009 publish succeeds. */
export function trackPendingPaymentRequest(
  state: PendingPaymentRequestsState,
  input: TrackPendingPaymentRequestInput,
): PendingPaymentRequestsState {
  const requestEventId = input.requestEventId.toLowerCase();
  if (state.verifiedIds.includes(requestEventId)) {
    return state;
  }
  const nextEntry: PendingPaymentRequestEntry = {
    requestEventId,
    channelId: input.channelId,
    amountMsat: input.amountMsat,
    bolt11: input.bolt11,
    paymentHash: input.paymentHash.toLowerCase(),
    createdAtUnix: input.createdAtUnix,
    expiryUnix: input.expiryUnix,
    lastUnconfirmableAtUnix: null,
  };
  const existingIdx = state.pending.findIndex(
    (entry) => entry.requestEventId === requestEventId,
  );
  if (existingIdx >= 0) {
    const pending = [...state.pending];
    pending[existingIdx] = {
      ...nextEntry,
      lastUnconfirmableAtUnix:
        state.pending[existingIdx].lastUnconfirmableAtUnix,
    };
    return { ...state, pending };
  }
  return { ...state, pending: [...state.pending, nextEntry] };
}

export function findPendingPaymentRequest(
  state: PendingPaymentRequestsState,
  requestEventId: string,
): PendingPaymentRequestEntry | null {
  const id = requestEventId.toLowerCase();
  return state.pending.find((entry) => entry.requestEventId === id) ?? null;
}

export function isVerifiedPaymentRequest(
  state: PendingPaymentRequestsState,
  requestEventId: string,
): boolean {
  return state.verifiedIds.includes(requestEventId.toLowerCase());
}

/** Drop expired pending entries. Returns removed ids for optional signals. */
export function dropExpiredPendingPaymentRequests(
  state: PendingPaymentRequestsState,
  nowUnix: number,
): { state: PendingPaymentRequestsState; expiredIds: string[] } {
  const expiredIds: string[] = [];
  const pending = state.pending.filter((entry) => {
    if (entry.expiryUnix > 0 && entry.expiryUnix <= nowUnix) {
      expiredIds.push(entry.requestEventId);
      return false;
    }
    return true;
  });
  if (expiredIds.length === 0) {
    return { state, expiredIds };
  }
  return { state: { ...state, pending }, expiredIds };
}

export type IncomingCheckStatus = "paid" | "unpaid" | "unconfirmable";

export type ApplyIncomingCheckResult =
  | {
      kind: "verified";
      state: PendingPaymentRequestsState;
      entry: PendingPaymentRequestEntry;
    }
  | {
      kind: "still_pending";
      state: PendingPaymentRequestsState;
    }
  | {
      kind: "unconfirmable";
      state: PendingPaymentRequestsState;
    }
  | {
      kind: "expired";
      state: PendingPaymentRequestsState;
      requestEventId: string;
    }
  | {
      kind: "already_verified";
      state: PendingPaymentRequestsState;
    }
  | {
      kind: "not_pending";
      state: PendingPaymentRequestsState;
    };

/**
 * Apply one walletCheckIncoming outcome to the registry.
 * paid → remove from pending + record verifiedIds (dedupe).
 * unpaid → keep pending.
 * unconfirmable → keep pending, stamp lastUnconfirmableAtUnix (rely on cadence).
 */
export function applyIncomingCheck(
  state: PendingPaymentRequestsState,
  input: {
    requestEventId: string;
    status: IncomingCheckStatus;
    nowUnix: number;
  },
): ApplyIncomingCheckResult {
  const requestEventId = input.requestEventId.toLowerCase();
  if (state.verifiedIds.includes(requestEventId)) {
    return { kind: "already_verified", state };
  }

  const entry = findPendingPaymentRequest(state, requestEventId);
  if (!entry) {
    return { kind: "not_pending", state };
  }

  if (entry.expiryUnix > 0 && entry.expiryUnix <= input.nowUnix) {
    const pending = state.pending.filter(
      (row) => row.requestEventId !== requestEventId,
    );
    return {
      kind: "expired",
      state: { ...state, pending },
      requestEventId,
    };
  }

  switch (input.status) {
    case "paid": {
      const pending = state.pending.filter(
        (row) => row.requestEventId !== requestEventId,
      );
      const verifiedIds = state.verifiedIds.includes(requestEventId)
        ? state.verifiedIds
        : [...state.verifiedIds, requestEventId];
      return {
        kind: "verified",
        state: { pending, verifiedIds },
        entry,
      };
    }
    case "unpaid":
      return { kind: "still_pending", state };
    case "unconfirmable": {
      const pending = state.pending.map((row) =>
        row.requestEventId === requestEventId
          ? { ...row, lastUnconfirmableAtUnix: input.nowUnix }
          : row,
      );
      return {
        kind: "unconfirmable",
        state: { ...state, pending },
      };
    }
    default: {
      const _exhaustive: never = input.status;
      return _exhaustive;
    }
  }
}
