/**
 * Composer "Request payment" affordance states.
 *
 * One discriminated union — not a pile of booleans — so rendering and
 * click/tooltip behavior stay exhaustive and obviously correct.
 */
export type RequestPaymentButtonState =
  | {
      kind: "connect_wallet";
      lookDisabled: true;
      tooltip: "Connect a wallet to request payment";
    }
  | {
      kind: "cannot_invoice";
      lookDisabled: true;
      tooltip: "Your wallet can't create invoices";
    }
  | {
      kind: "ready";
      lookDisabled: false;
      tooltip: "Request payment";
    };

export function deriveRequestPaymentButtonState(input: {
  linked: boolean;
  capabilities: readonly string[];
}): RequestPaymentButtonState {
  if (!input.linked) {
    return {
      kind: "connect_wallet",
      lookDisabled: true,
      tooltip: "Connect a wallet to request payment",
    };
  }
  if (!input.capabilities.includes("make_invoice")) {
    return {
      kind: "cannot_invoice",
      lookDisabled: true,
      tooltip: "Your wallet can't create invoices",
    };
  }
  return {
    kind: "ready",
    lookDisabled: false,
    tooltip: "Request payment",
  };
}
