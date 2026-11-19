import { REPORT_LIMITS, REPORT_REASONS, type ReportDraft } from "@/data/lib/catalyst/landings/report";

const DRAFT_KEY = "report_form_draft";
const DRAFT_TTL_MS = 15 * 60 * 1000;

type WrittenReport = Pick<ReportDraft, "reportedAddress" | "reason" | "description" | "additionalComments"> & {
  searchParams: string;
};

type SavedReport = WrittenReport & { savedAt: number };

function isSavedReport(value: unknown): value is SavedReport {
  if (!value || typeof value !== "object") return false;
  const draft = value as Partial<SavedReport>;
  return (
    typeof draft.searchParams === "string" &&
    typeof draft.reportedAddress === "string" &&
    typeof draft.description === "string" &&
    draft.description.length <= REPORT_LIMITS.descriptionMax &&
    typeof draft.additionalComments === "string" &&
    draft.additionalComments.length <= REPORT_LIMITS.commentsMax &&
    typeof draft.savedAt === "number" &&
    Number.isFinite(draft.savedAt) &&
    (draft.reason === "" || (draft.reason !== undefined && REPORT_REASONS.includes(draft.reason)))
  );
}

export function saveReportFormDraft(draft: WrittenReport): boolean {
  try {
    sessionStorage.setItem(DRAFT_KEY, JSON.stringify({ ...draft, savedAt: Date.now() }));
    return true;
  } catch {
    return false;
  }
}

export function takeReportFormDraft(searchParams: string): WrittenReport | null {
  try {
    const stored = sessionStorage.getItem(DRAFT_KEY);
    clearReportFormDraft();
    if (!stored) return null;

    const draft: unknown = JSON.parse(stored);
    if (!isSavedReport(draft)) return null;
    const age = Date.now() - draft.savedAt;
    if (age < 0 || age > DRAFT_TTL_MS) return null;
    if (searchParams && draft.searchParams !== searchParams) return null;
    return draft;
  } catch {
    return null;
  }
}

export function clearReportFormDraft(): void {
  try {
    sessionStorage.removeItem(DRAFT_KEY);
  } catch {
    // Storage can be unavailable for the whole browser session.
  }
}
