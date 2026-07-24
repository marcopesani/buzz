/**
 * Pure receipt-outbox state machine.
 *
 * Pending publishes survive app restart via the storage adapter; this module
 * only owns transitions. Dedupe key is always (requestEventId, paymentHash).
 */

export type ReceiptOutboxEntry = {
  requestEventId: string;
  paymentHash: string;
  preimage: string;
  amountMsat: number;
  /** Recovered from the 40009 `h` tag; null until resolve succeeds. */
  channelId: string | null;
  enqueuedAtUnix: number;
  /** Set when the entry is permanently dropped (e.g. request not found). */
  dropNote: string | null;
  lastError: string | null;
};

export type ReceiptOutboxState = {
  pending: ReceiptOutboxEntry[];
  /** Keys already published (or observed on the relay) — never publish again. */
  publishedKeys: string[];
};

export function receiptDedupeKey(
  requestEventId: string,
  paymentHash: string,
): string {
  return `${requestEventId.toLowerCase()}:${paymentHash.toLowerCase()}`;
}

export function emptyReceiptOutbox(): ReceiptOutboxState {
  return { pending: [], publishedKeys: [] };
}

export type EnqueueReceiptInput = {
  requestEventId: string;
  paymentHash: string;
  preimage: string;
  amountMsat: number;
  channelId?: string | null;
  nowUnix: number;
};

/**
 * Enqueue a settled pay-request receipt. No-op when already pending (and not
 * dropped) or already published for the same (request, payment_hash).
 * A previously dropped entry is revived — request-not-found at first reconcile
 * must not block a later successful resolve.
 */
export function enqueueReceipt(
  state: ReceiptOutboxState,
  input: EnqueueReceiptInput,
): ReceiptOutboxState {
  const key = receiptDedupeKey(input.requestEventId, input.paymentHash);
  if (state.publishedKeys.includes(key)) {
    return state;
  }
  const nextEntry: ReceiptOutboxEntry = {
    requestEventId: input.requestEventId.toLowerCase(),
    paymentHash: input.paymentHash.toLowerCase(),
    preimage: input.preimage.toLowerCase(),
    amountMsat: input.amountMsat,
    channelId: input.channelId ?? null,
    enqueuedAtUnix: input.nowUnix,
    dropNote: null,
    lastError: null,
  };
  const existingIdx = state.pending.findIndex(
    (entry) =>
      receiptDedupeKey(entry.requestEventId, entry.paymentHash) === key,
  );
  if (existingIdx >= 0) {
    if (state.pending[existingIdx].dropNote == null) {
      return state;
    }
    const pending = [...state.pending];
    pending[existingIdx] = nextEntry;
    return { ...state, pending };
  }
  return {
    ...state,
    pending: [...state.pending, nextEntry],
  };
}

/** Mark a receipt as successfully published (or observed on relay). */
export function ackReceipt(
  state: ReceiptOutboxState,
  requestEventId: string,
  paymentHash: string,
): ReceiptOutboxState {
  const key = receiptDedupeKey(requestEventId, paymentHash);
  const publishedKeys = state.publishedKeys.includes(key)
    ? state.publishedKeys
    : [...state.publishedKeys, key];
  return {
    pending: state.pending.filter(
      (entry) =>
        receiptDedupeKey(entry.requestEventId, entry.paymentHash) !== key,
    ),
    publishedKeys,
  };
}

/** Permanently drop an entry (request missing, invalid) — do not retry. */
export function dropReceipt(
  state: ReceiptOutboxState,
  requestEventId: string,
  paymentHash: string,
  note: string,
): ReceiptOutboxState {
  const key = receiptDedupeKey(requestEventId, paymentHash);
  return {
    ...state,
    pending: state.pending.map((entry) =>
      receiptDedupeKey(entry.requestEventId, entry.paymentHash) === key
        ? { ...entry, dropNote: note, lastError: null }
        : entry,
    ),
  };
}

/** Record a transient publish failure; entry stays retryable. */
export function markReceiptPublishError(
  state: ReceiptOutboxState,
  requestEventId: string,
  paymentHash: string,
  error: string,
): ReceiptOutboxState {
  const key = receiptDedupeKey(requestEventId, paymentHash);
  return {
    ...state,
    pending: state.pending.map((entry) =>
      receiptDedupeKey(entry.requestEventId, entry.paymentHash) === key
        ? { ...entry, lastError: error }
        : entry,
    ),
  };
}

/** Fill channelId after resolving the original 40009. */
export function setReceiptChannelId(
  state: ReceiptOutboxState,
  requestEventId: string,
  paymentHash: string,
  channelId: string,
): ReceiptOutboxState {
  const key = receiptDedupeKey(requestEventId, paymentHash);
  return {
    ...state,
    pending: state.pending.map((entry) =>
      receiptDedupeKey(entry.requestEventId, entry.paymentHash) === key
        ? { ...entry, channelId }
        : entry,
    ),
  };
}

/** Entries still eligible for a publish attempt. */
export function pendingReceiptsToRetry(
  state: ReceiptOutboxState,
): ReceiptOutboxEntry[] {
  return state.pending.filter((entry) => entry.dropNote == null);
}

export function isReceiptPublished(
  state: ReceiptOutboxState,
  requestEventId: string,
  paymentHash: string,
): boolean {
  return state.publishedKeys.includes(
    receiptDedupeKey(requestEventId, paymentHash),
  );
}
