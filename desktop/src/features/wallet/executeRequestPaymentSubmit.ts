/**
 * Orchestrates DM prepare → mint (once) → clamp expiry → build tags → publish.
 * Pure aside from injected deps — unit tests drive fakes.
 */

import { buildPaymentRequestEvent } from "./buildPaymentRequestEvent";
import { clampPaymentRequestExpiry } from "./clampPaymentRequestExpiry";
import { checkChannelEpoch, type MintedInvoice } from "./requestSubmitMachine";

export type ExecuteRequestPaymentSubmitDeps = {
  mode: "mint_and_publish" | "retry_publish";
  amountMsat: number;
  memo: string | null;
  requestedExpiryUnix: number | null;
  openedChannelId: string | null;
  /** Live channel id — read at each epoch check (survives mid-flight nav). */
  getCurrentChannelId: () => string | null;
  payeePubkey: string;
  /** Sticky invoice from a prior mint in this dialog session. */
  existingInvoice: MintedInvoice | null;
  /** Sticky channel from a prior mint+failed publish (retry must reuse it). */
  existingChannelId: string | null;
  prepareChannel?: () => Promise<string | null>;
  mintInvoice: (args: {
    amountMsat: number;
    description: string | null;
  }) => Promise<MintedInvoice>;
  publishEvent: (args: {
    channelId: string;
    kind: number;
    content: string;
    tags: string[][];
  }) => Promise<void>;
};

export type ExecuteRequestPaymentSubmitResult =
  | {
      ok: true;
      invoice: MintedInvoice;
      channelId: string;
      expiryUnix: number;
      clamped: boolean;
      tags: string[][];
    }
  | {
      ok: false;
      error: string;
      invoice: MintedInvoice | null;
      channelId: string | null;
      expiryUnix: number | null;
      amountMsat: number;
      memo: string | null;
      retryable: boolean;
    };

/**
 * Run one submit attempt. Caller owns the single-flight latch.
 *
 * Ordering: prepare DM channel (if needed) BEFORE minting, so the 40009 never
 * races channel creation and always has a valid `h` scope.
 */
export async function executeRequestPaymentSubmit(
  deps: ExecuteRequestPaymentSubmitDeps,
): Promise<ExecuteRequestPaymentSubmitResult> {
  const memo = deps.memo?.trim() ? deps.memo.trim() : null;

  let channelId: string | null =
    deps.mode === "retry_publish"
      ? (deps.existingChannelId ??
        deps.getCurrentChannelId() ??
        deps.openedChannelId)
      : (deps.getCurrentChannelId() ?? deps.openedChannelId);

  if (!channelId && deps.prepareChannel) {
    channelId = await deps.prepareChannel();
  }
  if (!channelId) {
    return {
      ok: false,
      error: "No channel available to publish the payment request.",
      invoice: deps.existingInvoice,
      channelId: null,
      expiryUnix: null,
      amountMsat: deps.amountMsat,
      memo,
      retryable: false,
    };
  }

  const epoch = checkChannelEpoch({
    openedChannelId: deps.openedChannelId,
    currentChannelId: deps.getCurrentChannelId(),
    publishChannelId: channelId,
  });
  if (!epoch.ok) {
    return {
      ok: false,
      error: epoch.reason,
      invoice: deps.existingInvoice,
      channelId,
      expiryUnix: null,
      amountMsat: deps.amountMsat,
      memo,
      retryable: false,
    };
  }

  let invoice: MintedInvoice;
  if (deps.mode === "retry_publish") {
    if (!deps.existingInvoice) {
      return {
        ok: false,
        error: "Nothing to retry — no invoice was minted.",
        invoice: null,
        channelId,
        expiryUnix: null,
        amountMsat: deps.amountMsat,
        memo,
        retryable: false,
      };
    }
    invoice = deps.existingInvoice;
  } else {
    try {
      invoice = await deps.mintInvoice({
        amountMsat: deps.amountMsat,
        description: memo,
      });
    } catch (err) {
      return {
        ok: false,
        error: err instanceof Error ? err.message : String(err),
        invoice: null,
        channelId,
        expiryUnix: null,
        amountMsat: deps.amountMsat,
        memo,
        retryable: false,
      };
    }
  }

  // Re-check epoch after await — community/channel may have switched mid-flight.
  const epochAfter = checkChannelEpoch({
    openedChannelId: deps.openedChannelId,
    currentChannelId: deps.getCurrentChannelId(),
    publishChannelId: channelId,
  });
  if (!epochAfter.ok) {
    return {
      ok: false,
      error: epochAfter.reason,
      invoice,
      channelId,
      expiryUnix: null,
      amountMsat: deps.amountMsat,
      memo,
      retryable: true,
    };
  }

  const { expiryUnix, clamped } = clampPaymentRequestExpiry({
    requestedExpiryUnix: deps.requestedExpiryUnix,
    invoiceExpiresAtUnix: invoice.expires_at_unix,
  });

  let built: ReturnType<typeof buildPaymentRequestEvent>;
  try {
    built = buildPaymentRequestEvent({
      channelId,
      amountMsat: deps.amountMsat,
      payeePubkey: deps.payeePubkey,
      bolt11: invoice.bolt11,
      memo,
      expiryUnix,
    });
  } catch (err) {
    return {
      ok: false,
      error: err instanceof Error ? err.message : String(err),
      invoice,
      channelId,
      expiryUnix,
      amountMsat: deps.amountMsat,
      memo,
      retryable: true,
    };
  }

  try {
    await deps.publishEvent({
      channelId,
      kind: built.kind,
      content: built.content,
      tags: built.tags,
    });
  } catch (err) {
    return {
      ok: false,
      error: err instanceof Error ? err.message : String(err),
      invoice,
      channelId,
      expiryUnix,
      amountMsat: deps.amountMsat,
      memo,
      retryable: true,
    };
  }

  return {
    ok: true,
    invoice,
    channelId,
    expiryUnix,
    clamped,
    tags: built.tags,
  };
}
