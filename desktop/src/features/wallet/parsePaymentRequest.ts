/**
 * Tag helpers for KIND_PAYMENT_REQUEST / KIND_PAYMENT_RECEIPT events.
 */

export type ParsedPaymentRequest = {
  amountMsat: number | null;
  memo: string | null;
  bolt11: string | null;
  lud16: string | null;
  /** Unix seconds from the `expiry` tag (not NIP-40 `expiration`). */
  expiryUnix: number | null;
  payeePubkey: string | null;
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

/** Parse payment-request tags from a timeline message / relay event. */
export function parsePaymentRequest(
  tags: string[][] | undefined,
): ParsedPaymentRequest {
  return {
    amountMsat: parsePositiveInt(tagValue(tags, "amount")),
    memo: tagValue(tags, "memo"),
    bolt11: tagValue(tags, "bolt11"),
    lud16: tagValue(tags, "lud16"),
    expiryUnix: parsePositiveInt(tagValue(tags, "expiry")),
    payeePubkey: tagValue(tags, "p"),
  };
}

/** `#e` target on a receipt — same last-e-tag scan reactions use. */
export function getReceiptRequestId(
  tags: string[][] | undefined,
): string | null {
  if (!tags) return null;
  for (let i = tags.length - 1; i >= 0; i -= 1) {
    const tag = tags[i];
    if (
      tag[0] === "e" &&
      typeof tag[1] === "string" &&
      tag[1].length === 64 &&
      /^[0-9a-f]+$/i.test(tag[1])
    ) {
      return tag[1].toLowerCase();
    }
  }
  return null;
}
