/**
 * Resolve → dedupe → build → sign+publish one receipt outbox entry.
 *
 * Confirm settle and reconcile flush share this path — the outbox is the only
 * source of pending publishes.
 */

import {
  KIND_PAYMENT_RECEIPT,
  KIND_PAYMENT_REQUEST,
} from "@/shared/constants/kinds";
import { getChannelIdFromTags } from "@/features/messages/lib/threading";
import type { RelayEvent } from "@/shared/api/types";

import { buildPaymentReceiptEvent } from "./buildPaymentReceiptEvent";
import { parsePaymentReceipt } from "./parsePaymentReceipt";
import { parsePaymentRequest } from "./parsePaymentRequest";
import {
  ackReceipt,
  dropReceipt,
  markReceiptPublishError,
  setReceiptChannelId,
  type ReceiptOutboxEntry,
  type ReceiptOutboxState,
} from "./receiptOutbox";

export type PublishPaymentReceiptDeps = {
  fetchRequestEvent: (requestEventId: string) => Promise<RelayEvent | null>;
  fetchReceiptsForRequest: (requestEventId: string) => Promise<RelayEvent[]>;
  publishEvent: (args: {
    kind: number;
    content: string;
    tags: string[][];
  }) => Promise<void>;
  log?: (message: string) => void;
};

export type PublishPaymentReceiptResult =
  | { kind: "published" }
  | { kind: "already_on_relay" }
  | { kind: "already_acked" }
  | { kind: "dropped"; note: string }
  | { kind: "retry"; error: string };

function receiptMatchesHash(event: RelayEvent, paymentHash: string): boolean {
  const parsed = parsePaymentReceipt(event.tags);
  return parsed.paymentHash === paymentHash.toLowerCase();
}

/**
 * Attempt one publish for a pending outbox entry. Returns the next outbox
 * state and a result discriminant for callers/tests.
 */
export async function publishPaymentReceiptEntry(
  state: ReceiptOutboxState,
  entry: ReceiptOutboxEntry,
  deps: PublishPaymentReceiptDeps,
): Promise<{ state: ReceiptOutboxState; result: PublishPaymentReceiptResult }> {
  const log = deps.log ?? ((msg: string) => console.warn(msg));

  if (entry.dropNote != null) {
    return { state, result: { kind: "dropped", note: entry.dropNote } };
  }

  let channelId = entry.channelId;
  const amountMsat = entry.amountMsat;

  if (!channelId) {
    let request: RelayEvent | null;
    try {
      request = await deps.fetchRequestEvent(entry.requestEventId);
    } catch (err) {
      const error = err instanceof Error ? err.message : String(err);
      log(
        `[wallet-receipt] request fetch failed for ${entry.requestEventId}: ${error}`,
      );
      return {
        state: markReceiptPublishError(
          state,
          entry.requestEventId,
          entry.paymentHash,
          error,
        ),
        result: { kind: "retry", error },
      };
    }
    if (!request || request.kind !== KIND_PAYMENT_REQUEST) {
      const note = "request_not_found";
      log(
        `[wallet-receipt] dropping receipt — ${note} for ${entry.requestEventId}`,
      );
      return {
        state: dropReceipt(
          state,
          entry.requestEventId,
          entry.paymentHash,
          note,
        ),
        result: { kind: "dropped", note },
      };
    }
    if (request.id.toLowerCase() !== entry.requestEventId.toLowerCase()) {
      const note = "request_id_mismatch";
      log(
        `[wallet-receipt] dropping receipt — ${note} for ${entry.requestEventId}`,
      );
      return {
        state: dropReceipt(
          state,
          entry.requestEventId,
          entry.paymentHash,
          note,
        ),
        result: { kind: "dropped", note },
      };
    }
    channelId = getChannelIdFromTags(request.tags);
    if (!channelId) {
      const note = "request_missing_h";
      log(
        `[wallet-receipt] dropping receipt — ${note} for ${entry.requestEventId}`,
      );
      return {
        state: dropReceipt(
          state,
          entry.requestEventId,
          entry.paymentHash,
          note,
        ),
        result: { kind: "dropped", note },
      };
    }
    const parsedRequest = parsePaymentRequest(request.tags);
    if (
      parsedRequest.amountMsat != null &&
      parsedRequest.amountMsat > 0 &&
      parsedRequest.amountMsat !== amountMsat
    ) {
      // Prefer the wallet settlement amount; log the mismatch as a courtesy.
      log(
        `[wallet-receipt] amount mismatch request=${parsedRequest.amountMsat} settled=${amountMsat} for ${entry.requestEventId}`,
      );
    }
    state = setReceiptChannelId(
      state,
      entry.requestEventId,
      entry.paymentHash,
      channelId,
    );
  }

  let existing: RelayEvent[];
  try {
    existing = await deps.fetchReceiptsForRequest(entry.requestEventId);
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    return {
      state: markReceiptPublishError(
        state,
        entry.requestEventId,
        entry.paymentHash,
        error,
      ),
      result: { kind: "retry", error },
    };
  }

  if (existing.some((ev) => receiptMatchesHash(ev, entry.paymentHash))) {
    return {
      state: ackReceipt(state, entry.requestEventId, entry.paymentHash),
      result: { kind: "already_on_relay" },
    };
  }

  let built: ReturnType<typeof buildPaymentReceiptEvent>;
  try {
    built = buildPaymentReceiptEvent({
      channelId,
      requestEventId: entry.requestEventId,
      paymentHash: entry.paymentHash,
      preimage: entry.preimage,
      amountMsat,
    });
  } catch (err) {
    const note = err instanceof Error ? err.message : String(err);
    log(`[wallet-receipt] dropping invalid receipt: ${note}`);
    return {
      state: dropReceipt(state, entry.requestEventId, entry.paymentHash, note),
      result: { kind: "dropped", note },
    };
  }

  try {
    await deps.publishEvent({
      kind: built.kind,
      content: built.content,
      tags: built.tags,
    });
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    return {
      state: markReceiptPublishError(
        state,
        entry.requestEventId,
        entry.paymentHash,
        error,
      ),
      result: { kind: "retry", error },
    };
  }

  return {
    state: ackReceipt(state, entry.requestEventId, entry.paymentHash),
    result: { kind: "published" },
  };
}

/** Flush every retryable pending entry through {@link publishPaymentReceiptEntry}. */
export async function flushReceiptOutbox(
  state: ReceiptOutboxState,
  deps: PublishPaymentReceiptDeps,
): Promise<ReceiptOutboxState> {
  let next = state;
  const snapshot = [...next.pending.filter((e) => e.dropNote == null)];
  for (const entry of snapshot) {
    const outcome = await publishPaymentReceiptEntry(next, entry, deps);
    next = outcome.state;
  }
  return next;
}

/** Filter helpers shared with production wiring (kinds + e/ids for p-gate). */
export function paymentRequestByIdFilter(requestEventId: string) {
  return {
    ids: [requestEventId],
    kinds: [KIND_PAYMENT_REQUEST],
    limit: 1,
  };
}

export function paymentReceiptsByRequestFilter(requestEventId: string) {
  return {
    kinds: [KIND_PAYMENT_RECEIPT],
    "#e": [requestEventId],
    limit: 20,
  };
}
