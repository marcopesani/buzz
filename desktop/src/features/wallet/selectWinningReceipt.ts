/**
 * Deterministic Paid ✓ winner among decorative receipts for one request.
 * Lowest `created_at` wins; event-id string compare is the tiebreak.
 */

export type ReceiptCandidate = {
  id: string;
  createdAt: number;
};

/** Pure selection — order of `receipts` does not matter. */
export function selectWinningReceipt(
  receipts: readonly ReceiptCandidate[],
): ReceiptCandidate | null {
  if (receipts.length === 0) {
    return null;
  }
  let winner = receipts[0];
  for (let i = 1; i < receipts.length; i += 1) {
    const candidate = receipts[i];
    if (
      candidate.createdAt < winner.createdAt ||
      (candidate.createdAt === winner.createdAt && candidate.id < winner.id)
    ) {
      winner = candidate;
    }
  }
  return winner;
}
