/**
 * Pure KIND_PAYMENT_RECEIPT (40010) event shape.
 *
 * Tag schema matches `buzz_sdk::build_payment_receipt` / CLI / timeline join:
 * `h`, bare `e` (request id), `payment_hash`, `preimage`, `amount` (msat).
 * Content is always empty. No `p` tag — payer is the author; payee is the
 * referenced request.
 */

export type BuildPaymentReceiptEventInput = {
  channelId: string;
  requestEventId: string;
  paymentHash: string;
  preimage: string;
  amountMsat: number;
};

export type BuiltPaymentReceiptEvent = {
  kind: 40010;
  content: "";
  tags: string[][];
};

const HEX_64 = /^[0-9a-f]{64}$/i;

/** Build an unsigned 40010 payload ready for `signRelayEvent` + publish. */
export function buildPaymentReceiptEvent(
  input: BuildPaymentReceiptEventInput,
): BuiltPaymentReceiptEvent {
  if (!Number.isSafeInteger(input.amountMsat) || input.amountMsat <= 0) {
    throw new Error("amount_msat must be a positive integer");
  }
  if (!input.channelId) {
    throw new Error("channelId is required");
  }
  if (!HEX_64.test(input.requestEventId)) {
    throw new Error("requestEventId must be 64-char hex");
  }
  if (!HEX_64.test(input.paymentHash)) {
    throw new Error("paymentHash must be 64-char hex");
  }
  if (!HEX_64.test(input.preimage)) {
    throw new Error("preimage must be 64-char hex");
  }

  return {
    kind: 40010,
    content: "",
    tags: [
      ["h", input.channelId],
      ["e", input.requestEventId.toLowerCase()],
      ["payment_hash", input.paymentHash.toLowerCase()],
      ["preimage", input.preimage.toLowerCase()],
      ["amount", String(input.amountMsat)],
    ],
  };
}
