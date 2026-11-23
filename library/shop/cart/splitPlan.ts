// How Credits and MANA pay one cart together: which lines Credits pay whole (K), which one line, if any, Credits and MANA
// pay together (S), and which lines MANA pays (M). Deterministic and in cart order; it is not the best split, only a stable
// one. The host runs the parts and works out S's MANA amounts from the sale's exact price; this module only places lines.
//
// A line's cost in Credits is what partitionCart charges for it: each unit's quote up to the next whole credit, times its
// units. S's Credits part is the whole credits left, against the line's unit quote as quoted (not rounded up).

import { addCents, chargeForQuote, mulCents, subCents, wholeCreditsFloor, ZERO_CENTS, type ManaWei, type UsdCents } from "../model/money";
import { offerQuote } from "../model/purchase";
import type { PaymentMethod, Session } from "../model/settlement";
import { lineUnits, partitionCart, type LineRefusal, type PricedLine } from "./partition";

export type CartSplitLine = {
  lineId: string;
  /** What Credits pay for it: whole credits only, more than nothing and less than `priceCents`. */
  legCents: UsdCents;
  /** The line's Credits quote for its one unit, not rounded up: the price the credit is authorized against. */
  priceCents: UsdCents;
};

export type CartSplitPlan = {
  /** Lines Credits pay whole, in cart order. */
  credits: string[];
  /** The one line Credits and MANA pay together: a single unit the server marks `splittable`. */
  split: CartSplitLine | null;
  /** Lines MANA pays whole, in cart order. */
  mana: string[];
  /** What the plan takes from the Credits balance: the `credits` lines' charge plus `split.legCents`. */
  creditsCents: UsdCents;
};

export type CartSplitRefusal =
  | { reason: "empty" }
  | { reason: "duplicateLine"; lineId: string }
  /** Neither Credits nor MANA can pay this line: the cart has no split. */
  | { reason: "noMethod"; lineId: string }
  /** The lines only Credits can pay cost more than the balance. */
  | { reason: "creditsShort"; required: UsdCents; balance: UsdCents }
  /** The parts, partitioned as checkout would, refuse these lines (a Credits group over its unit cap, say). */
  | { reason: "refused"; refusals: LineRefusal[] }
  /** Credits pay nothing: the cart pays as it does without a split. */
  | { reason: "noCreditsPart" }
  /** Credits pay everything: the cart pays as it does without a split. */
  | { reason: "noManaPart" }
  /** The MANA lines alone (S's MANA part not counted) cost more than the wallet holds. */
  | { reason: "manaShort"; required: ManaWei; balance: ManaWei };

export type CartSplitResult = { ok: true; plan: CartSplitPlan } | { ok: false; refusal: CartSplitRefusal };

export type CartSplitBalances = {
  /** The spendable Credits balance, held credits already taken off. */
  creditsBalance: UsdCents;
  /** The wallet's MANA, for a first check of the MANA lines; the host quotes them for real. */
  manaBalance?: ManaWei | null;
};

// The cart's own partition already reports an inconsistent offer; these partitions would only repeat it.
const quiet = { report: () => {} };

function as(entry: PricedLine, method: PaymentMethod): PricedLine {
  return { line: { ...entry.line, method }, offer: entry.offer };
}

function pays(entry: PricedLine, method: PaymentMethod, session: Session): boolean {
  return partitionCart([as(entry, method)], session, quiet).ok;
}

/** The line's cost in Credits as partitionCart charges it: each unit rounded up to a whole credit. */
function creditsCost(entry: PricedLine): UsdCents | null {
  const quote = entry.offer ? offerQuote(entry.offer, "credits") : null;
  const charge = quote?.method === "credits" ? chargeForQuote(quote.quoted) : null;
  return charge === null ? null : mulCents(charge, lineUnits(entry.line));
}

export function planCartSplit(priced: PricedLine[], session: Session, balances: CartSplitBalances): CartSplitResult {
  const no = (refusal: CartSplitRefusal): CartSplitResult => ({ ok: false, refusal });
  if (priced.length === 0) return no({ reason: "empty" });
  const ids = new Set<string>();
  for (const { line } of priced) {
    if (ids.has(line.id)) return no({ reason: "duplicateLine", lineId: line.id });
    ids.add(line.id);
  }

  const lanes = priced.map((entry) => {
    const cost = pays(entry, "credits", session) ? creditsCost(entry) : null;
    return { entry, cost, manaPays: pays(entry, "mana", session) };
  });
  const none = lanes.find((l) => l.cost === null && !l.manaPays);
  if (none) return no({ reason: "noMethod", lineId: none.entry.line.id });

  // Lines only Credits can pay come first: without them covered there is no split.
  const balance = balances.creditsBalance > 0 ? balances.creditsBalance : ZERO_CENTS;
  let required = ZERO_CENTS;
  for (const { cost, manaPays } of lanes) {
    if (cost !== null && !manaPays) required = addCents(required, cost) ?? (Number.MAX_SAFE_INTEGER as UsdCents);
  }
  if (required > balance) return no({ reason: "creditsShort", required, balance });
  let left = subCents(balance, required)!;

  const credits: PricedLine[] = [];
  const mana: PricedLine[] = [];
  let split: CartSplitLine | null = null;
  // Then, in cart order: a line whose cost fits what is left goes to Credits. The first that does not fit and can be split
  // takes the whole credits left, and MANA pays every line after it. One that cannot be split (several units, a price the
  // node does not split) goes to MANA, and the walk goes on.
  for (const { entry, cost, manaPays } of lanes) {
    if (cost !== null && !manaPays) {
      credits.push(entry);
      continue;
    }
    if (cost === null || split) {
      mana.push(entry);
      continue;
    }
    if (cost <= left) {
      credits.push(entry);
      left = subCents(left, cost)!;
      continue;
    }
    const quote = entry.offer ? offerQuote(entry.offer, "credits") : null;
    const leg = wholeCreditsFloor(left);
    if (lineUnits(entry.line) === 1 && entry.offer?.splittable === true && quote?.method === "credits" && leg > 0 && leg < quote.quoted) {
      split = { lineId: entry.line.id, legCents: leg, priceCents: quote.quoted };
      left = subCents(left, leg)!;
      continue;
    }
    mana.push(entry);
  }

  if (credits.length === 0 && !split) return no({ reason: "noCreditsPart" });
  if (mana.length === 0 && !split) return no({ reason: "noManaPart" });

  const cartIndex = new Map(priced.map((p, i) => [p.line.id, i]));
  const inCart = (r: LineRefusal): LineRefusal => ({ ...r, index: cartIndex.get(r.lineId) ?? r.index });
  const byCredits = partitionCart(credits.map((e) => as(e, "credits")), session, quiet);
  const byMana = partitionCart(mana.map((e) => as(e, "mana")), session, quiet);
  const refusals = [...byCredits.refused, ...byMana.refused].map(inCart).sort((a, b) => a.index - b.index);
  if (refusals.length > 0) return no({ reason: "refused", refusals });

  const manaBalance = balances.manaBalance;
  if (manaBalance != null && byMana.totals.mana > manaBalance) {
    return no({ reason: "manaShort", required: byMana.totals.mana, balance: manaBalance });
  }

  const creditsCents = addCents(byCredits.totals.credits.charged, split?.legCents ?? ZERO_CENTS)!;
  const lineIds = (lines: PricedLine[]) => lines.map((e) => e.line.id);
  return { ok: true, plan: { credits: lineIds(credits), split, mana: lineIds(mana), creditsCents } };
}
