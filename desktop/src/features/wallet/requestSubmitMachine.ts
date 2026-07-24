/**
 * Submit lifecycle for the request-payment dialog.
 *
 * One record drives mint → publish → retry. Retry never remints; only a fresh
 * dialog session (reset) clears the sticky invoice. Single-flight is a ref-backed
 * latch so React re-renders cannot double-fire.
 */

export type MintedInvoice = {
  bolt11: string;
  payment_hash: string;
  expires_at_unix: number;
};

export type RequestSubmitPhase =
  | { kind: "form" }
  | { kind: "busy" }
  | {
      kind: "retryable";
      invoice: MintedInvoice;
      amountMsat: number;
      memo: string | null;
      expiryUnix: number;
      channelId: string;
      error: string;
    }
  | { kind: "success" };

export type RequestSubmitAction =
  | { type: "begin" }
  | { type: "abort_to_form" }
  | {
      type: "publish_failed";
      invoice: MintedInvoice;
      amountMsat: number;
      memo: string | null;
      expiryUnix: number;
      channelId: string;
      error: string;
    }
  | { type: "published" }
  | { type: "begin_retry" }
  | { type: "reset" };

/** Reduce dialog submit phase. Invalid transitions are no-ops. */
export function reduceRequestSubmit(
  state: RequestSubmitPhase,
  action: RequestSubmitAction,
): RequestSubmitPhase {
  switch (action.type) {
    case "begin":
      if (state.kind !== "form") return state;
      return { kind: "busy" };
    case "abort_to_form":
      if (state.kind !== "busy") return state;
      return { kind: "form" };
    case "publish_failed":
      if (state.kind !== "busy") return state;
      return {
        kind: "retryable",
        invoice: action.invoice,
        amountMsat: action.amountMsat,
        memo: action.memo,
        expiryUnix: action.expiryUnix,
        channelId: action.channelId,
        error: action.error,
      };
    case "published":
      if (state.kind !== "busy") return state;
      return { kind: "success" };
    case "begin_retry":
      if (state.kind !== "retryable") return state;
      return { kind: "busy" };
    case "reset":
      return { kind: "form" };
    default: {
      const _exhaustive: never = action;
      return _exhaustive;
    }
  }
}

/** Ref-survivable single-flight latch for mint+publish. */
export function createSubmitLatch() {
  let locked = false;
  return {
    tryEnter(): boolean {
      if (locked) return false;
      locked = true;
      return true;
    },
    exit(): void {
      locked = false;
    },
    isLocked(): boolean {
      return locked;
    },
  };
}

export type ChannelEpochCheckInput = {
  /** Channel id captured when the dialog opened (null for unprepared DM). */
  openedChannelId: string | null;
  /** Live channel id from props at check time. */
  currentChannelId: string | null;
  /** Channel id we are about to publish into. */
  publishChannelId: string;
};

/** Abort when open/submit context no longer matches the publish target. */
export function checkChannelEpoch(
  input: ChannelEpochCheckInput,
): { ok: true } | { ok: false; reason: string } {
  const reason = "Channel changed while the dialog was open.";
  if (
    input.openedChannelId != null &&
    input.publishChannelId !== input.openedChannelId
  ) {
    return { ok: false, reason };
  }
  if (
    input.openedChannelId != null &&
    input.currentChannelId != null &&
    input.currentChannelId !== input.openedChannelId
  ) {
    return { ok: false, reason };
  }
  if (
    input.openedChannelId == null &&
    input.currentChannelId != null &&
    input.currentChannelId !== input.publishChannelId
  ) {
    return { ok: false, reason };
  }
  return { ok: true };
}
