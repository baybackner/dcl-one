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
import { readContract, rpc } from "./rpc";
import { isTxPending, sendCall, TxPendingError, waitForReceipt, walletAccount, type WaitOptions } from "./tx";

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
 * An approval this page sent whose end it has not seen (its wait ran out), per chain, owner and spender. Kept until
 * its own receipt says it mined or reverted, never released by time: while it stands, no second approval goes out
 * for that spender (it would only cost a second fee), and a purchase the allowance does not cover stops on its hash.
 * It lives in this page's memory: an approval dropped or replaced without a receipt holds a short allowance back until
 * the page is reloaded.
 */
const unconfirmedApprovals = new Map<string, string>();
/** One approval decision at a time per key: the tail of the decisions queued on it. */
const approvalQueue = new Map<string, Promise<unknown>>();
const approvalKey = (chainId: ChainId, owner: string, spender: string) => `${chainId}:${owner}:${spender.toLowerCase()}`;

/** How long one reconciling read (an earlier approval's receipt, the allowance after it) may take. */
const RECONCILE_READ_MS = 15_000;

export function resetUnconfirmedApprovalsForTests(): void {
  unconfirmedApprovals.clear();
  approvalQueue.clear();
}

function bounded<T>(read: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error("The read took too long.")), ms);
  });
  return Promise.race([read, late]).finally(() => clearTimeout(timer));
}

/** An earlier approval's receipt, read once within `ms`: no receipt, or no answer in time, is "unknown". */
async function approvalOutcome(chainId: ChainId, hash: string, ms: number): Promise<"mined" | "reverted" | "unknown"> {
  try {
    const receipt = await bounded(rpc<{ status?: string } | null>(chainId, "eth_getTransactionReceipt", [hash]), ms);
    return receipt?.status === "0x1" ? "mined" : receipt?.status === "0x0" ? "reverted" : "unknown";
  } catch {
    return "unknown";
  }
}

/** Runs `decide` after every decision already queued on `key`; queued synchronously, so two callers never interleave. */
function serialized<T>(key: string, decide: () => Promise<T>): Promise<T> {
  const mine = (approvalQueue.get(key) ?? Promise.resolve()).then(decide, decide);
  const tail = mine.catch(() => {});
  approvalQueue.set(key, tail);
  void tail.then(() => {
    if (approvalQueue.get(key) === tail) approvalQueue.delete(key);
  });
  return mine;
}

/**
 * Approves `spender` for `amount` MANA when the current allowance is short. Resolves null when nothing was
 * needed, else the confirmed hash. The exact amount, never an unlimited allowance. Decisions for one chain, owner
 * and spender run one after another, each reading its own allowance. While an earlier approval for that spender has
 * not been seen to end, a purchase its allowance does not cover throws `TxPendingError` with the earlier hash.
 */
export async function ensureManaAllowance(opts: {
  provider: Eip1193Provider;
  chainId: ChainId;
  spender: string;
  amount: bigint;
  wait?: WaitOptions;
  /** Bound on each reconciling read; tests shorten it. */
  readTimeoutMs?: number;
}): Promise<string | null> {
  const owner = await walletAccount(opts.provider);
  const key = approvalKey(opts.chainId, owner, opts.spender);
  return serialized(key, () => decideApproval(opts, owner, key));
}

async function decideApproval(
  opts: { provider: Eip1193Provider; chainId: ChainId; spender: string; amount: bigint; wait?: WaitOptions; readTimeoutMs?: number },
  owner: string,
  key: string,
): Promise<string | null> {
  const earlier = unconfirmedApprovals.get(key);
  if (earlier) {
    const ms = opts.readTimeoutMs ?? RECONCILE_READ_MS;
    if ((await approvalOutcome(opts.chainId, earlier, ms)) === "unknown") {
      // A covering allowance lets this purchase pass, but says nothing about the earlier approval: it stays recorded.
      const covered = await bounded(hasManaAllowance(opts.chainId, owner, opts.spender, opts.amount), ms).catch(() => false);
      if (covered) return null;
      throw new TxPendingError(earlier, "The MANA approval");
    }
    // Mined or reverted: it no longer holds a second approval back.
    if (unconfirmedApprovals.get(key) === earlier) unconfirmedApprovals.delete(key);
  }
  if (await hasManaAllowance(opts.chainId, owner, opts.spender, opts.amount)) return null;
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
    if (unconfirmedApprovals.get(key) === hash) unconfirmedApprovals.delete(key);
    return confirmed;
  } catch (e) {
    // Reverted: ended. Still pending: kept for the next decision to reconcile.
    if (!isTxPending(e) && unconfirmedApprovals.get(key) === hash) unconfirmedApprovals.delete(key);
    throw e;
  }
}
