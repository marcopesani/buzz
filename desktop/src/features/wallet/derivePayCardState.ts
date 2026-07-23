import { normalizePubkey } from "@/shared/lib/pubkey";

/**
 * Single derivation for pay-card UI state.
 *
 * JSX switches on this result — do not scatter expiry/own/linked booleans
 * across components.
 */

export type PayCardState =
  | { kind: "paid" }
  | { kind: "expired" }
  | { kind: "own_request" }
  | { kind: "unlinked" }
  | { kind: "payable" };

export type DerivePayCardStateInput = {
  requestPubkey: string;
  myPubkey: string | null | undefined;
  /** Unix seconds from the `expiry` tag; null when absent. */
  expiryUnix: number | null;
  nowUnix: number;
  hasReceipt: boolean;
  walletLinked: boolean;
};

/**
 * Priority: paid → expired → own request → unlinked → payable.
 * Agent badge / owner attribution is orthogonal (message author metadata).
 */
export function derivePayCardState(
  input: DerivePayCardStateInput,
): PayCardState {
  if (input.hasReceipt) {
    return { kind: "paid" };
  }
  if (input.expiryUnix != null && input.nowUnix >= input.expiryUnix) {
    return { kind: "expired" };
  }
  const mine = input.myPubkey
    ? normalizePubkey(input.requestPubkey) === normalizePubkey(input.myPubkey)
    : false;
  if (mine) {
    return { kind: "own_request" };
  }
  if (!input.walletLinked) {
    return { kind: "unlinked" };
  }
  return { kind: "payable" };
}

/** Whether the Pay action should be shown for this derived state. */
export function payCardShowsPayButton(state: PayCardState): boolean {
  switch (state.kind) {
    case "payable":
      return true;
    case "paid":
    case "expired":
    case "own_request":
    case "unlinked":
      return false;
    default: {
      const _exhaustive: never = state;
      return _exhaustive;
    }
  }
}
