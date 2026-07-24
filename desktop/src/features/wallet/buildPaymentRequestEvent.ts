/**
 * Pure KIND_PAYMENT_REQUEST (40009) event shape.
 *
 * Tag schema matches `buzz_sdk::build_payment_request` / `buzz wallet request`
 * and what `parsePaymentRequest` / PaymentRequestCard consume:
 * `h`, `amount` (msat), `p` (payee), `bolt11`, optional `memo`, optional `expiry`.
 * Content is always empty — description lives in the `memo` tag.
 */

export type BuildPaymentRequestEventInput = {
  channelId: string;
  amountMsat: number;
  payeePubkey: string;
  bolt11: string;
  memo?: string | null;
  expiryUnix?: number | null;
};

export type BuiltPaymentRequestEvent = {
  kind: 40009;
  content: "";
  tags: string[][];
};

/** Build an unsigned 40009 payload ready for `signRelayEvent` + publish. */
export function buildPaymentRequestEvent(
  input: BuildPaymentRequestEventInput,
): BuiltPaymentRequestEvent {
  if (!Number.isSafeInteger(input.amountMsat) || input.amountMsat <= 0) {
    throw new Error("amount_msat must be a positive integer");
  }
  if (!input.channelId) {
    throw new Error("channelId is required");
  }
  if (!input.payeePubkey) {
    throw new Error("payeePubkey is required");
  }
  if (!input.bolt11) {
    throw new Error("bolt11 is required");
  }

  const tags: string[][] = [
    ["h", input.channelId],
    ["amount", String(input.amountMsat)],
    ["p", input.payeePubkey],
    ["bolt11", input.bolt11],
  ];
  const memo = input.memo?.trim();
  if (memo) {
    tags.push(["memo", memo]);
  }
  if (input.expiryUnix != null) {
    tags.push(["expiry", String(input.expiryUnix)]);
  }

  return {
    kind: 40009,
    content: "",
    tags,
  };
}
