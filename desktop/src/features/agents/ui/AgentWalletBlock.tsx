import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";

import {
  fetchAgentWalletStatus,
  provisionManagedAgentWallet,
  unprovisionManagedAgentWallet,
} from "@/features/agents/agentWalletApi";
import {
  AGENT_WALLET_UNLINK_WARNING,
  deriveAgentWalletBlockState,
  type AgentWalletMutationPhase,
} from "@/features/agents/deriveAgentWalletBlockState";
import { managedAgentsQueryKey } from "@/features/agents/hooks";
import { FeatureGate } from "@/shared/features";
import { cn } from "@/shared/lib/cn";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/shared/ui/alert-dialog";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/shared/ui/tooltip";

import {
  PERSONA_FIELD_CONTROL_CLASS,
  PERSONA_FIELD_SHELL_CLASS,
} from "./agentConfigOptions";

type AgentWalletBlockProps = {
  agentPubkey: string;
  agentRunning: boolean;
  agentWorking: boolean;
  disabled?: boolean;
  needsRestart: boolean;
  onRestart: () => void;
};

/**
 * Lightning Wallet link/unlink block for the agent edit Advanced section.
 * Hidden entirely when the `wallet` experiment is off.
 */
export function AgentWalletBlock(props: AgentWalletBlockProps) {
  return (
    <FeatureGate feature="wallet">
      <AgentWalletBlockInner {...props} />
    </FeatureGate>
  );
}

function AgentWalletBlockInner({
  agentPubkey,
  agentRunning,
  agentWorking,
  disabled = false,
  needsRestart,
  onRestart,
}: AgentWalletBlockProps) {
  const queryClient = useQueryClient();
  const [nwcUri, setNwcUri] = React.useState("");
  const [mutation, setMutation] =
    React.useState<AgentWalletMutationPhase>("idle");
  const [probeError, setProbeError] = React.useState<string | null>(null);
  const [confirmUnlink, setConfirmUnlink] = React.useState(false);
  const [unlinkError, setUnlinkError] = React.useState<string | null>(null);

  const statusQuery = useQuery({
    queryKey: ["agent-wallet-status", agentPubkey],
    queryFn: () => fetchAgentWalletStatus(agentPubkey),
  });

  const state = deriveAgentWalletBlockState({
    status: statusQuery.data ?? null,
    statusLoading: statusQuery.isLoading,
    mutation,
    probeError,
    confirmUnlink,
    agentWorking,
    needsRestart,
    agentRunning,
  });

  const refreshAgentList = async () => {
    await queryClient.invalidateQueries({ queryKey: managedAgentsQueryKey });
  };

  const handleLink = async () => {
    if (state.kind !== "unlinked" || state.actionsGuarded || disabled) return;
    const trimmed = nwcUri.trim();
    if (!trimmed) return;
    setMutation("probing");
    setProbeError(null);
    try {
      await provisionManagedAgentWallet(agentPubkey, trimmed);
      setNwcUri("");
      await statusQuery.refetch();
      await refreshAgentList();
    } catch (error) {
      setProbeError(error instanceof Error ? error.message : String(error));
    } finally {
      setMutation("idle");
    }
  };

  const handleConfirmUnlink = async () => {
    if (disabled) return;
    setMutation("unlinking");
    setUnlinkError(null);
    try {
      await unprovisionManagedAgentWallet(agentPubkey);
      setConfirmUnlink(false);
      await statusQuery.refetch();
      await refreshAgentList();
    } catch (error) {
      setUnlinkError(error instanceof Error ? error.message : String(error));
    } finally {
      setMutation("idle");
    }
  };

  const actionsDisabled =
    disabled ||
    (state.kind === "unlinked" || state.kind === "linked"
      ? state.actionsGuarded
      : state.kind === "keyring_unavailable" ||
        state.kind === "loading" ||
        state.kind === "probing" ||
        state.kind === "unlinking");

  const guardTooltip =
    state.kind === "unlinked" || state.kind === "linked"
      ? state.guardTooltip
      : null;

  return (
    <div className="space-y-1.5" data-testid="agent-wallet-block">
      <p className="text-sm font-medium text-foreground">Lightning Wallet</p>
      <p className="text-xs text-muted-foreground">
        Link a receive-only Nostr Wallet Connect URI for this agent. The secret
        is stored in the OS keyring and never shown again.
      </p>

      {renderBody({
        state,
        nwcUri,
        setNwcUri,
        actionsDisabled,
        guardTooltip,
        disabled,
        onLink: () => void handleLink(),
        onRequestUnlink: () => setConfirmUnlink(true),
        onRestart,
        unlinkError,
      })}

      <AlertDialog
        onOpenChange={(open) => {
          if (!open && mutation !== "unlinking") {
            setConfirmUnlink(false);
            setUnlinkError(null);
          }
        }}
        open={confirmUnlink}
      >
        <AlertDialogContent data-testid="agent-wallet-unlink-confirm">
          <AlertDialogHeader>
            <AlertDialogTitle>Unlink agent wallet?</AlertDialogTitle>
            <AlertDialogDescription>
              {AGENT_WALLET_UNLINK_WARNING}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {unlinkError ? (
            <p className="text-xs text-destructive">{unlinkError}</p>
          ) : null}
          <AlertDialogFooter>
            <Button
              disabled={mutation === "unlinking"}
              onClick={() => setConfirmUnlink(false)}
              size="sm"
              type="button"
              variant="outline"
            >
              Cancel
            </Button>
            <Button
              data-testid="agent-wallet-unlink-confirm-submit"
              disabled={mutation === "unlinking"}
              onClick={() => void handleConfirmUnlink()}
              size="sm"
              type="button"
              variant="destructive"
            >
              {mutation === "unlinking" ? "Unlinking…" : "Unlink wallet"}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function renderBody({
  state,
  nwcUri,
  setNwcUri,
  actionsDisabled,
  guardTooltip,
  disabled,
  onLink,
  onRequestUnlink,
  onRestart,
  unlinkError,
}: {
  state: ReturnType<typeof deriveAgentWalletBlockState>;
  nwcUri: string;
  setNwcUri: (value: string) => void;
  actionsDisabled: boolean;
  guardTooltip: string | null;
  disabled: boolean;
  onLink: () => void;
  onRequestUnlink: () => void;
  onRestart: () => void;
  unlinkError: string | null;
}) {
  switch (state.kind) {
    case "loading":
      return (
        <p
          className="text-xs text-muted-foreground"
          data-testid="agent-wallet-loading"
        >
          Checking wallet state…
        </p>
      );
    case "keyring_unavailable":
      return (
        <p
          className="text-xs text-amber-600 dark:text-amber-400"
          data-testid="agent-wallet-keyring-unavailable"
        >
          Wallet state unavailable — keyring locked/unreachable
        </p>
      );
    case "probing":
      return (
        <div className="space-y-2" data-testid="agent-wallet-probing">
          <div
            className={cn(
              "flex min-h-11 items-center px-3 opacity-60",
              PERSONA_FIELD_SHELL_CLASS,
            )}
          >
            <Input
              autoComplete="off"
              className={cn(
                "h-8 px-0 py-0 leading-6",
                PERSONA_FIELD_CONTROL_CLASS,
              )}
              disabled
              spellCheck={false}
              type="password"
              value={nwcUri}
            />
          </div>
          <Button disabled size="sm" type="button">
            Linking…
          </Button>
        </div>
      );
    case "unlinking":
      return (
        <div className="space-y-2" data-testid="agent-wallet-unlinking">
          <LinkedIndicator />
          <Button disabled size="sm" type="button" variant="destructive">
            Unlinking…
          </Button>
          {state.showRestartCta ? <RestartCta onRestart={onRestart} /> : null}
        </div>
      );
    case "unlinked":
      return (
        <div className="space-y-2" data-testid="agent-wallet-unlinked">
          <div
            className={cn(
              "flex min-h-11 items-center px-3",
              PERSONA_FIELD_SHELL_CLASS,
            )}
          >
            <Input
              autoComplete="off"
              className={cn(
                "h-8 px-0 py-0 leading-6",
                PERSONA_FIELD_CONTROL_CLASS,
              )}
              data-testid="agent-wallet-nwc-uri"
              disabled={disabled || state.actionsGuarded}
              onChange={(event) => setNwcUri(event.target.value)}
              placeholder="nostr+walletconnect://…"
              spellCheck={false}
              type="password"
              value={nwcUri}
            />
          </div>
          {state.error ? (
            <p
              className="text-xs text-destructive"
              data-testid="agent-wallet-error"
            >
              {state.error}
            </p>
          ) : null}
          <GuardedAction
            ariaLabel="Link wallet"
            disabled={actionsDisabled || !nwcUri.trim()}
            softDisabled={state.actionsGuarded}
            testId="agent-wallet-link"
            tooltip={guardTooltip}
            onClick={onLink}
          >
            Link wallet
          </GuardedAction>
          {state.showRestartCta ? <RestartCta onRestart={onRestart} /> : null}
        </div>
      );
    case "linked":
      return (
        <div className="space-y-2" data-testid="agent-wallet-linked">
          <LinkedIndicator />
          {unlinkError ? (
            <p className="text-xs text-destructive">{unlinkError}</p>
          ) : null}
          <GuardedAction
            ariaLabel="Unlink wallet"
            disabled={actionsDisabled}
            softDisabled={state.actionsGuarded}
            testId="agent-wallet-unlink"
            tooltip={guardTooltip}
            variant="destructive"
            onClick={onRequestUnlink}
          >
            Unlink wallet
          </GuardedAction>
          {state.showRestartCta ? <RestartCta onRestart={onRestart} /> : null}
        </div>
      );
    default: {
      const _exhaustive: never = state;
      return _exhaustive;
    }
  }
}

function LinkedIndicator() {
  return (
    <p
      className="text-sm font-medium text-emerald-600 dark:text-emerald-400"
      data-testid="agent-wallet-linked-badge"
    >
      Wallet linked
    </p>
  );
}

function RestartCta({ onRestart }: { onRestart: () => void }) {
  return (
    <div
      className="flex flex-wrap items-center gap-2"
      data-testid="agent-wallet-restart-cta"
    >
      <p className="text-xs text-amber-600 dark:text-amber-400">
        Configuration changed since this agent started. Restart to apply it.
      </p>
      <Button
        data-testid="agent-wallet-restart"
        onClick={onRestart}
        size="sm"
        type="button"
        variant="outline"
      >
        <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
        Restart to apply
      </Button>
    </div>
  );
}

function GuardedAction({
  ariaLabel,
  children,
  disabled,
  onClick,
  softDisabled,
  testId,
  tooltip,
  variant = "default",
}: {
  ariaLabel: string;
  children: React.ReactNode;
  disabled: boolean;
  onClick: () => void;
  softDisabled: boolean;
  testId: string;
  tooltip: string | null;
  variant?: "default" | "destructive";
}) {
  const button = (
    <Button
      aria-disabled={softDisabled || undefined}
      aria-label={ariaLabel}
      className={cn(softDisabled && "opacity-50")}
      data-testid={testId}
      disabled={disabled && !softDisabled}
      onClick={(event) => {
        if (softDisabled || disabled) {
          event.preventDefault();
          return;
        }
        onClick();
      }}
      size="sm"
      type="button"
      variant={variant}
    >
      {children}
    </Button>
  );

  if (!tooltip) {
    return button;
  }

  return (
    <Tooltip disableHoverableContent>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent>{tooltip}</TooltipContent>
    </Tooltip>
  );
}
