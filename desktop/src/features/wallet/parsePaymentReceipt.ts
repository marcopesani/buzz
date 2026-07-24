/**
 * Tag helpers for KIND_PAYMENT_RECEIPT (40010) events.
 */

import { getReceiptRequestId } from "./parsePaymentRequest";

export type ParsedPaymentReceipt = {
  requestEventId: string | null;
  paymentHash: string | null;
  preimage: string | null;
  amountMsat: number | null;
  channelId: string | null;
};

function tagValue(tags: string[][] | undefined, name: string): string | null {
  if (!tags) return null;
  for (const tag of tags) {
    if (tag[0] === name && typeof tag[1] === "string" && tag[1].length > 0) {
      return tag[1];
    }
  }
  return null;
}

function parsePositiveInt(raw: string | null): number | null {
  if (raw == null) return null;
  if (!/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  if (!Number.isSafeInteger(n) || n < 0) return null;
  return n;
}

/** Parse payment-receipt tags the way the timeline / pay-card consume them. */
export function parsePaymentReceipt(
  tags: string[][] | undefined,
): ParsedPaymentReceipt {
  return {
    requestEventId: getReceiptRequestId(tags),
    paymentHash: tagValue(tags, "payment_hash")?.toLowerCase() ?? null,
    preimage: tagValue(tags, "preimage")?.toLowerCase() ?? null,
    amountMsat: parsePositiveInt(tagValue(tags, "amount")),
    channelId: tagValue(tags, "h"),
  };
}
