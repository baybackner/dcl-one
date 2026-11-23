// from: shop@94584a5c:app/src/lib/authorizations.ts (getManaSpendingAuthorization, needsApprovalStep),
//       app/src/hooks/useManaBalance.ts; marketplace@f3956826:webapp/src/modules/bid/utils.ts (isInsufficientMANA)
// Copyright 2020 Metaverse Holdings Ltd. (marketplace), Apache-2.0.
// MANA the wallet holds and has allowed a contract to spend, read from the target chain, and the one
// allowance a MANA bid or a MANA purchase needs before it can settle. On Polygon the approval is relayed
// (MANA (PoS) supports native meta-transactions); on Ethereum the wallet sends it and pays gas.

import { encodeFunctionData } from "viem";

import type { Eip1193Provider } from "../../../data/auth/wallet";
import { isPolygon, type ChainId } from "@shop/model/chain";
import type { ManaWei } from "@shop/model/money";
import { ERC20_ABI, manaToken } from "@shop/market/lib/contracts";
import { sendMetaTransaction } from "./metaTx";
import { readContract } from "./rpc";
import { isTxPending, isTxReverted, sendCall, waitForReceipt, walletAccount, type WaitOptions } from "./tx";

export { parseManaInput } from "@shop/model/money";

export async function readManaBalance(chainId: ChainId, owner: string): Promise<ManaWei> {
  const mana = manaToken(chainId);
  return (await readContract({ chainId, address: mana.address, abi: ERC20_ABI, functionName: "balanceOf", args: [owner] })) as ManaWei;
}

export async function readManaAllowance(chainId: ChainId, owner: string, spender: string): Promise<bigint> {
  const mana = manaToken(chainId);
  return (await readContract({ chainId, address: mana.address, abi: ERC20_ABI, functionName: "allowance", args: [owner, spender] })) as bigint;
}

/** Whether `spender` may already move `amount` of the owner's MANA (no approval step needed). */
export async function hasManaAllowance(chainId: ChainId, owner: string, spender: string, amount: bigint): Promise<boolean> {
  return (await readManaAllowance(chainId, owner, spender)) >= amount;
}

/**
 * An approval this page sent that had not confirmed when its wait ran out, per chain, owner and spender. The next
 * approval for that spender reads its receipt first: while it is unmined no second approval is sent (it would only
 * cost a second fee), and the purchase stops on the earlier one's hash instead.
 */
const unconfirmedApprovals = new Map<string, string>();
const approvalKey = (chainId: ChainId, owner: string, spender: string) => `${chainId}:${owner}:${spender.toLowerCase()}`;

export function resetUnconfirmedApprovalsForTests(): void {
  unconfirmedApprovals.clear();
}

/**
 * Approves `spender` for `amount` MANA when the current allowance is short. Resolves null when nothing was
 * needed, else the confirmed hash. The exact amount, never an unlimited allowance. An earlier approval for the
 * same spender that has not confirmed yet throws `TxPendingError` with its hash, after one read of its receipt.
 */
export async function ensureManaAllowance(opts: {
  provider: Eip1193Provider;
  chainId: ChainId;
  spender: string;
  amount: bigint;
  wait?: WaitOptions;
}): Promise<string | null> {
  const owner = await walletAccount(opts.provider);
  const key = approvalKey(opts.chainId, owner, opts.spender);
  if (await hasManaAllowance(opts.chainId, owner, opts.spender, opts.amount)) {
    unconfirmedApprovals.delete(key);
    return null;
  }
  const earlier = unconfirmedApprovals.get(key);
  if (earlier) {
    try {
      // One read, no wait: unmined (or unreadable) answers TxPendingError with the earlier hash.
      await waitForReceipt(opts.chainId, earlier, "The MANA approval", { ...opts.wait, timeoutMs: 0 });
    } catch (e) {
      if (!isTxReverted(e)) throw e;
    }
    // Mined or reverted, it no longer holds a second approval back; mined, it may have been enough.
    unconfirmedApprovals.delete(key);
    if (await hasManaAllowance(opts.chainId, owner, opts.spender, opts.amount)) return null;
  }
  const mana = manaToken(opts.chainId);
  const hash = isPolygon(opts.chainId)
    ? await sendMetaTransaction({
        provider: opts.provider,
        contract: mana,
        kind: "collection",
        functionData: encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [opts.spender as `0x${string}`, opts.amount] }),
      })
    : await sendCall(opts.provider, { chainId: opts.chainId, to: mana.address, abi: ERC20_ABI, functionName: "approve", args: [opts.spender, opts.amount] });
  unconfirmedApprovals.set(key, hash);
  try {
    const confirmed = await waitForReceipt(opts.chainId, hash, "The MANA approval", opts.wait);
    unconfirmedApprovals.delete(key);
    return confirmed;
  } catch (e) {
    if (!isTxPending(e)) unconfirmedApprovals.delete(key);
    throw e;
  }
}
