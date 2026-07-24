import { Zap } from "lucide-react";

import { useAppShell } from "@/app/AppShellContext";
import { FeatureGate } from "@/shared/features";
import { cn } from "@/shared/lib/cn";
import { Button } from "@/shared/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/shared/ui/tooltip";

import { deriveRequestPaymentButtonState } from "./deriveRequestPaymentButtonState";
import { useWalletStatus } from "./useWalletStatus";

type RequestPaymentButtonProps = {
  composerDisabled?: boolean;
  onRequestPayment?: () => void;
};

/**
 * Collapsed-composer Zap control for payment requests.
 *
 * Hidden entirely when the `wallet` experiment is off. When visible, a single
 * derived state drives tooltip + click: connect (deep-link settings), cannot
 * mint invoices (noop), or ready (optional `onRequestPayment`).
 *
 * Uses `aria-disabled` (not `disabled`) for soft-disabled states so tooltips
 * and the connect deep-link keep working — the shared Button's `disabled`
 * sets pointer-events-none.
 */
export function RequestPaymentButton({
  composerDisabled = false,
  onRequestPayment,
}: RequestPaymentButtonProps) {
  return (
    <FeatureGate feature="wallet">
      <RequestPaymentButtonInner
        composerDisabled={composerDisabled}
        onRequestPayment={onRequestPayment}
      />
    </FeatureGate>
  );
}

function RequestPaymentButtonInner({
  composerDisabled,
  onRequestPayment,
}: {
  composerDisabled: boolean;
  onRequestPayment?: () => void;
}) {
  const { status } = useWalletStatus();
  const { onOpenSettings } = useAppShell();
  const state = deriveRequestPaymentButtonState({
    linked: status.linked,
    capabilities: status.capabilities,
  });

  const handleClick = () => {
    if (composerDisabled) return;
    switch (state.kind) {
      case "connect_wallet":
        onOpenSettings?.("wallet");
        return;
      case "cannot_invoice":
        return;
      case "ready":
        onRequestPayment?.();
        return;
      default: {
        const _exhaustive: never = state;
        return _exhaustive;
      }
    }
  };

  // Real disabled only when the whole composer is inert — matches mention /
  // paperclip. Soft-disabled wallet states stay clickable for tooltip + nudge.
  const hardDisabled = composerDisabled;
  const softDisabled = !hardDisabled && state.lookDisabled;

  return (
    <Tooltip disableHoverableContent>
      <TooltipTrigger asChild>
        <Button
          aria-disabled={softDisabled || undefined}
          aria-label={state.tooltip}
          className={cn(softDisabled && "opacity-50 text-muted-foreground")}
          data-testid="request-payment-button"
          disabled={hardDisabled}
          onClick={handleClick}
          size="icon"
          type="button"
          variant="ghost"
        >
          <Zap />
        </Button>
      </TooltipTrigger>
      <TooltipContent>{state.tooltip}</TooltipContent>
    </Tooltip>
  );
}
