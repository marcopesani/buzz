/**
 * Event `expiry` must never exceed the minted invoice's `expires_at_unix`.
 * Null requested expiry means "use the invoice expiry" (CLI default).
 */

export type ClampPaymentRequestExpiryInput = {
  /** Unix seconds the user asked for; null = match invoice. */
  requestedExpiryUnix: number | null;
  invoiceExpiresAtUnix: number;
};

export type ClampPaymentRequestExpiryResult = {
  expiryUnix: number;
  clamped: boolean;
};

/** Clamp a requested event expiry so the card cannot outlive the bolt11. */
export function clampPaymentRequestExpiry(
  input: ClampPaymentRequestExpiryInput,
): ClampPaymentRequestExpiryResult {
  const requested = input.requestedExpiryUnix ?? input.invoiceExpiresAtUnix;
  if (requested > input.invoiceExpiresAtUnix) {
    return {
      expiryUnix: input.invoiceExpiresAtUnix,
      clamped: true,
    };
  }
  return {
    expiryUnix: requested,
    clamped: false,
  };
}
