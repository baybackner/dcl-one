import { useEventsCopy, type EventsLocale } from "@dcl/one-ui/apps/sites/events/locale";
import { EventDetailModal } from "@dcl/one-ui/apps/sites/events/DetailModal";
import { GeneralAdd, GeneralDelete, GeneralGallery, GeneralInfoButton, GeneralLeftArrow, GeneralShowPreview } from "@dcl/one-ui/foundations/icons";
import { Button } from "@dcl/one-ui";
import { Button as SitesButton } from "@dcl/one-ui/apps/sites/shared/Button/Button";
import { Notice, type NoticeTone } from "@dcl/one-ui/apps/sites/events/Notice";
import { useEffect, useRef, useState, type DragEvent, type ReactNode } from "react";
import { useMachine } from "@xstate/react";
import { Link, useSearchParams } from "react-router";


import type { TrackContext } from "@/core/lib/telemetry/track";
import { HANGOUT_BOUNDS, isStepValid, toEventSchedule, type EventScheduleSource, type HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import type { HangoutOptions } from "@/data/lib/catalyst/landings/hangout-options";
import { heroWhen, scheduleLine } from "../../components/event-schedule";
import styles from "./SubmitHangout.module.css";
import HangoutAssociations, { AdminFields, CommunityField } from "./HangoutAssociations";
import FeaturedAssetPicker from "./FeaturedAssetPicker";
import SelectControl from "./SelectControl";
import SubmitSuccess from "./SubmitSuccess";
import {
  hangoutMachine,
  HANGOUT_EVENTS,
  emitStepCompleted,
  resolveHangoutSnapshot,
  slugToState,
  stateToSlug,
  FORM_ORDER,
  type HangoutStateId,
  type SubmitFn,
  type TrackFn,
} from "./machine";

export type CategoryOption = { name: string; label: string; i18n?: Partial<Record<EventsLocale, string | null>> };
type UploadFn = (file: File, vertical: boolean, signal?: AbortSignal) => Promise<string>;

type SubmitHangoutProps = {
  trackCtx: TrackContext;
  categories: CategoryOption[];
  categoriesError?: boolean;
  mode?: "create" | "edit";
  draft?: HangoutDraft;
  originalSchedule?: EventScheduleSource;
  initialStep?: string;
  submit?: SubmitFn;
  track?: TrackFn;
  signedIn?: boolean;
  onSignIn?: () => void;
  upload?: UploadFn;
  options?: HangoutOptions | null;
  canEditAny?: boolean;
  privateFieldsEditable?: boolean;
  canChangeCommunity?: boolean;
  editor?: boolean;
  reviewNotice?: string;
  /** How the notice reads: a rejection is an error, a note on an approved event a warning. */
  reviewTone?: NoticeTone;
  /** Edit mode: opens the delete confirmation from the form's actions. */
  onDelete?: () => void;
};

export default function SubmitHangout({
  trackCtx,
  categories,
  categoriesError = false,
  mode = "create",
  draft,
  originalSchedule,
  initialStep,
  submit,
  track,
  signedIn = false,
  onSignIn,
  upload,
  options = null,
  canEditAny = false,
  privateFieldsEditable = mode === "create",
  canChangeCommunity = mode === "create",
  editor = false,
  reviewNotice,
  reviewTone = "warning",
  onDelete,
}: SubmitHangoutProps) {
  const [searchParams] = useSearchParams();

  const urlStep = (searchParams.get("step")?.trim() || initialStep) ?? undefined;
  const inline = editor && !urlStep;
  const stateId = inline && signedIn ? "review" : slugToState(urlStep);

  return (
    <SubmitHangoutInner
      stateId={stateId}
      inline={inline}
      reviewNotice={reviewNotice}
      reviewTone={reviewTone}
      trackCtx={trackCtx}
      categories={categories}
      categoriesError={categoriesError}
      mode={mode}
      draft={draft}
      originalSchedule={originalSchedule}
      submit={submit}
      track={track}
      signedIn={signedIn}
      onSignIn={onSignIn}
      upload={upload}
      options={options}
      canEditAny={canEditAny}
      privateFieldsEditable={privateFieldsEditable}
      canChangeCommunity={canChangeCommunity}
      onDelete={onDelete}
    />
  );
}

type InnerProps = {
  stateId: HangoutStateId;
  inline: boolean;
  reviewNotice?: string;
  /** How the notice reads: a rejection is an error, a note on an approved event a warning. */
  reviewTone?: NoticeTone;
  trackCtx: TrackContext;
  categories: CategoryOption[];
  categoriesError: boolean;
  mode: "create" | "edit";
  draft?: HangoutDraft;
  originalSchedule?: EventScheduleSource;
  submit?: SubmitFn;
  track?: TrackFn;
  signedIn: boolean;
  onSignIn?: () => void;
  upload?: UploadFn;
  options: HangoutOptions | null;
  canEditAny: boolean;
  privateFieldsEditable: boolean;
  canChangeCommunity: boolean;
  onDelete?: () => void;
};

function uiStateFor(value: HangoutStateId): "signin" | "success" | "form" {
  if (value === "signinGate") return "signin";
  if (value === "submitted") return "success";
  return "form";
}

function SubmitHangoutInner({
  stateId,
  inline,
  reviewNotice,
  reviewTone = "warning",
  trackCtx,
  categories,
  categoriesError,
  mode,
  draft,
  originalSchedule,
  submit,
  track,
  signedIn,
  onSignIn,
  upload,
  options,
  canEditAny,
  privateFieldsEditable,
  canChangeCommunity,
  onDelete,
}: InnerProps) {
  const { locale, text, errorText } = useEventsCopy();
  const [, setSearchParams] = useSearchParams();

  const snapshot = useRef(
    resolveHangoutSnapshot({ step: stateId, trackCtx, draft, submit, track }),
  ).current;

  const [state, send] = useMachine(hangoutMachine, {
    input: { trackCtx, draft, submit, track },
    snapshot,
  });

  const value = state.value as HangoutStateId;
  const step = stateToSlug(value);
  const d = state.context.draft;
  const emitTrack: TrackFn = track ?? state.context.track;
  const needsSignIn = Boolean(onSignIn) && !signedIn;
  const trackDirectEntry = useRef(inline && stateId === "review");
  useEffect(() => {
    if (!trackDirectEntry.current) return;
    trackDirectEntry.current = false;
    emitTrack(HANGOUT_EVENTS.started, {}, trackCtx);
  }, [emitTrack, trackCtx]);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const uploadController = useRef<AbortController | null>(null);
  useEffect(() => () => uploadController.current?.abort(), []);
  async function uploadImage(file: File, vertical: boolean) {
    if (!upload) return;
    uploadController.current?.abort();
    const controller = new AbortController();
    uploadController.current = controller;
    setUploading(true);
    setUploadError(null);
    try {
      const url = await upload(file, vertical, controller.signal);
      if (!controller.signal.aborted) send({ type: "EDIT", patch: vertical ? { verticalImagePreviewUrl: url } : { imagePreviewUrl: url } });
    } catch (error) {
      if (!controller.signal.aborted) setUploadError(error instanceof Error ? error.message : "Could not upload the image.");
    } finally {
      if (!controller.signal.aborted) setUploading(false);
    }
  }
  useEffect(() => {
    if (signedIn && state.matches("signinGate")) send({ type: "SIGN_IN", inline });
  }, [inline, signedIn, state, send]);

  const lastStep = useRef<string | null>(null);
  useEffect(() => {
    if (inline || lastStep.current === step) return;
    lastStep.current = step;
    setSearchParams(
      (prev) => {
        const params = new URLSearchParams(prev);
        if (params.get("step") === step) return params;
        params.set("step", step);
        return params;
      },
      { replace: true, preventScrollReset: true },
    );
  }, [inline, step, setSearchParams]);

  const isForm = !needsSignIn && uiStateFor(value) === "form";
  const formStepIndex = FORM_ORDER.indexOf(value);
  const communityAllowed = !options || Boolean(options.communitiesError) || !d.communityId || (mode === "edit" && d.communityId === draft?.communityId) || Boolean(options?.communities.some((community) => community.id === d.communityId));
  const canAdvanceForm = !uploading && (value === "cover" || (isStepValid(value, d) && (value !== "details" || communityAllowed)));

  const editingInline = inline && (value === "review" || value === "preview");
  const previewSchedule = inline && value === "preview"
    ? { ...toEventSchedule(d, originalSchedule), finish_at: null, next_start_at: null, next_finish_at: null }
    : null;
  const canSubmit = !uploading && communityAllowed &&
    FORM_ORDER.every((section) => isStepValid(section, d));
  const onEdit = (patch: Partial<HangoutDraft>) => send({ type: "EDIT", patch });
  const uploadFeedback = uploadError ? <Notice tone="error">{errorText(uploadError)}</Notice>
    : uploading ? <Notice>{text("Uploading image…")}</Notice> : null;

  const onUpload = upload ? (file: File, vertical: boolean) => { void uploadImage(file, vertical); } : undefined;
  const communityNotice = !communityAllowed && <Notice tone="error">{text("Choose a community you manage, or select None.")}</Notice>;

  function fields(section: HangoutStateId) {
    return (
      <>
        <StepFields
          step={section}
          draft={d}
          categories={categories}
          categoriesError={categoriesError}
          onEdit={onEdit}
          onUpload={onUpload}
          uploading={uploading}
          privateFieldsEditable={privateFieldsEditable}
          options={options}
        />
        {section === "details" && (
          <HangoutAssociations draft={d} options={options} canEditAny={canEditAny} canChangeCommunity={canChangeCommunity} onEdit={onEdit} />
        )}
        {section === "details" && communityNotice}
      </>
    );
  }

  return (
    <div className={inline ? `submit-hangout ${styles.authoring}` : "submit-hangout"} data-editor={inline || undefined} data-step={inline ? undefined : step} data-state={value}>
      {inline ? <EditorHeader mode={mode} /> : (
        <header className="submit-hangout__header">
          <Link to="/events">{text("Back to Events")}</Link>
          <h1>{text(mode === "edit" ? "Edit Hangout" : "Submit a Hangout")}</h1>
        </header>
      )}

      <div className={`submit-hangout__controls ${inline ? styles.editorControls : ""}`} role="group" aria-label={text(inline ? "Event editor" : "Hangout wizard")}>
        {(value === "signinGate" || needsSignIn) && (
          <button
            type="button"
            className="submit-hangout__btn submit-hangout__btn--primary"
            onClick={() => onSignIn ? onSignIn() : send({ type: "SIGN_IN", inline })}
          >
            {text("Sign In to continue")}
          </button>
        )}

        {isForm && (
          <>
            {editingInline ? (
              <div className={styles.editorColumns}>
                <div className={styles.mediaPanel}>
                  <fieldset className={styles.editorSection}>
                    <legend>{text("Cover")}</legend>
                    <CoverFields draft={d} uploading={uploading} onUpload={onUpload} onEdit={onEdit} />
                  </fieldset>
                  {uploadFeedback}
                  <DescriptionFields draft={d} categories={categories} categoriesError={categoriesError} privateFieldsEditable={privateFieldsEditable} onEdit={onEdit} />
                </div>
                <div className={styles.detailsPanel}>
                  <div className={styles.sections}>
                    {reviewNotice && <Notice tone={reviewTone}>{reviewNotice}</Notice>}
                    {canEditAny && (
                      <fieldset className={styles.adminArea}>
                        <legend>{text("Admin area")}</legend>
                        <AdminFields draft={d} options={options} onEdit={onEdit} />
                      </fieldset>
                    )}
                    <h2>{text("Hangout Details")}</h2>
                    <ScheduleFields draft={d} onEdit={onEdit} />
                    <LocationFields draft={d} options={options} onEdit={onEdit} />
                    <div className={styles.row}>
                      <CommunityField className={styles.grow} draft={d} options={options} canChangeCommunity={canChangeCommunity} onEdit={onEdit} />
                      {privateFieldsEditable && (
                        <Field label={text("Contact email")} className={styles.grow}>
                          <input className={styles.control} type="email" value={d.contact} placeholder="hello@example.com" onChange={(event) => onEdit({ contact: event.target.value })} />
                        </Field>
                      )}
                    </div>
                    {communityNotice}
                    <FeaturedAssetPicker value={d.featuredItem ?? ""} onChange={(featuredItem) => onEdit({ featuredItem })} />
                  </div>
                  <div className={styles.actionBar}>
                    <div className={styles.reviewNote}>
                      <p><GeneralInfoButton size={16} aria-hidden="true" />{text("The hangout submission will be reviewed and approved by our team, you'll be notified by email.")}</p>
                      <SitesButton variant="tertiary" size="sm" className={styles.previewButton} disabled={!canSubmit} onClick={() => send({ type: "PREVIEW" })}>
                        {text("Preview")}<GeneralShowPreview size={16} aria-hidden="true" />
                      </SitesButton>
                    </div>
                    <hr className={styles.divider} />
                    <div className={styles.formActions}>
                      {mode === "edit" && onDelete && (
                        <button type="button" className={styles.delete} onClick={onDelete}>
                          {text("Delete")}<GeneralDelete size={24} aria-hidden="true" />
                        </button>
                      )}
                      <div className={styles.mainActions}>
                        <Link className={styles.cancel} to={mode === "edit" ? "/events?tab=my" : "/events"}>{text("Cancel")}</Link>
                        <Button variant="primary" size="lg" viewport="desktop" disabled={!canSubmit} onClick={() => send({ type: "SUBMIT" })}>
                          {text(mode === "edit" ? "Save changes" : "Submit Event")}
                        </Button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            ) : fields(value)}
            {previewSchedule && (
              <EventDetailModal
                title={d.name}
                image={d.imagePreviewUrl}
                live={false}
                when={heroWhen(previewSchedule, undefined, locale)}
                schedule={scheduleLine(previewSchedule, undefined, locale)}
                location={{ world: d.location === "world", label: d.location === "world" ? d.worldName : `${d.coordX},${d.coordY}` }}
                description={d.description}
                onClose={() => send({ type: "BACK" })}
              />
            )}
            {!editingInline && uploadFeedback}
            {!editingInline && <div className="submit-hangout__nav">
              {value === "cover" ? <Link className="submit-hangout__btn" to="/events">{text("Back")}</Link> : <button
                type="button"
                className="submit-hangout__btn"
                onClick={() => send({ type: "BACK" })}
              >
                {text("Back")}
              </button>}

              {value === "review" ? (
                <>
                  <button
                    type="button"
                    className="submit-hangout__btn"
                    onClick={() => send({ type: "PREVIEW" })}
                  >
                    {text("Preview")}
                  </button>
                  <button
                    type="button"
                    className="submit-hangout__btn submit-hangout__btn--primary"
                    onClick={() => send({ type: "SUBMIT" })}
                  >
                    {text("Submit Hangout")}
                  </button>
                </>
              ) : value === "preview" ? (
                <button
                  type="button"
                  className="submit-hangout__btn submit-hangout__btn--primary"
                  onClick={() => send({ type: "SUBMIT" })}
                >
                  {text("Confirm & Submit")}
                </button>
              ) : (
                <ForwardButton
                  from={value}
                  enabled={canAdvanceForm}
                  send={send}
                  emitTrack={emitTrack}
                  trackCtx={state.context.trackCtx}
                  nextStep={FORM_ORDER[formStepIndex + 1]}
                />
              )}
            </div>}
          </>
        )}

        {value === "submitting" && (
          <Notice>{text("Submitting your hangout for review…")}</Notice>
        )}
        {value === "submitted" && !needsSignIn && <SubmitSuccess mode={mode} approved={state.context.result?.approved ?? false} />}

        {state.context.error && value === "review" && (
          <Notice tone="error">
            {text("Submit failed: {error}. Please try again.", { error: errorText(state.context.error) })}
          </Notice>
        )}
      </div>
    </div>
  );
}

function EditorHeader({ mode }: { mode: "create" | "edit" }) {
  const { text } = useEventsCopy();
  return (
    <header className={`submit-hangout__header ${styles.editorHeader}`}>
      <Link to="/events" className={styles.back} aria-label={text("Back to Events")}><GeneralLeftArrow size={30} aria-hidden="true" /></Link>
      <h1>{text(mode === "edit" ? "Edit hangout" : "Submit a hangout")}</h1>
    </header>
  );
}

type EditFn = (patch: Partial<HangoutDraft>) => void;
const DESCRIPTION_MAX = HANGOUT_BOUNDS.descriptionMax;
const RECURRENCES = [
  ["every_hour", "Every hour"],
  ["every_day", "Every day"],
  ["every_week", "Every week"],
  ["every_month", "Every month"],
  ["every_year", "Every year"],
] as const;

/**
 * One labelled control of the editor: the title above, the control, and an optional line under it that
 * stays outside the label so it is not read as part of the control's name.
 */
function Field({ label, className, footer, children }: { label: string; className?: string; footer?: ReactNode; children: ReactNode }) {
  const control = (
    <label className={[styles.fieldLabel, footer ? undefined : className].filter(Boolean).join(" ")}>
      <span className={styles.fieldTitle}>{label}</span>
      {children}
    </label>
  );
  if (!footer) return control;
  return <div className={[styles.fieldGroup, className].filter(Boolean).join(" ")}>{control}{footer}</div>;
}

function CoverFields({ draft, uploading, onUpload, onEdit }: { draft: HangoutDraft; uploading: boolean; onUpload?: (file: File, vertical: boolean) => void; onEdit: EditFn }) {
  const { text } = useEventsCopy();
  const [verticalCoverOpen, setVerticalCoverOpen] = useState(Boolean(draft.verticalImagePreviewUrl));
  return (
    <div className={styles.coverFields}>
      <CoverField inline image={draft.imagePreviewUrl} uploading={uploading} onUpload={onUpload} onRemove={() => onEdit({ imagePreviewUrl: null })} />
      <p className={styles.note}><GeneralInfoButton size={16} aria-hidden="true" />{text("Upload a PNG, JPG, or GIF (1340×670px, max 500kb). Center visuals and text to fit all screen resolutions.")}</p>
      <details className={styles.verticalCover} open={verticalCoverOpen} onToggle={(event) => setVerticalCoverOpen(event.currentTarget.open)}>
        <summary><GeneralAdd size={16} aria-hidden="true" /><strong>{text("Add Vertical Cover")}</strong> {text("(recommended)")}</summary>
        <CoverField inline vertical image={draft.verticalImagePreviewUrl} uploading={uploading} onUpload={onUpload} onRemove={() => onEdit({ verticalImagePreviewUrl: null })} />
      </details>
    </div>
  );
}

function DescriptionFields({ draft, categories, categoriesError, privateFieldsEditable, onEdit }: {
  draft: HangoutDraft; categories: CategoryOption[]; categoriesError: boolean; privateFieldsEditable: boolean; onEdit: EditFn;
}) {
  const { locale, text } = useEventsCopy();
  return (
    <fieldset className={styles.editorSection}>
      <legend>{text("Event details")}</legend>
      <div className={styles.stack}>
        <Field label={text("Hangout name")}>
          <input className={styles.control} type="text" maxLength={HANGOUT_BOUNDS.nameMax} value={draft.name} placeholder={text("Be as descriptive as you can")} onChange={(event) => onEdit({ name: event.target.value })} />
        </Field>
        <Field label={text("Description")} footer={<span className={styles.counter}>{draft.description.length}/{DESCRIPTION_MAX}</span>}>
          <textarea className={styles.control} rows={4} maxLength={DESCRIPTION_MAX} value={draft.description} placeholder={text("Describe what you'll be doing")} onChange={(event) => onEdit({ description: event.target.value })} />
        </Field>
        {categories.length > 0 ? (
          <Field label={text("Category")}>
            <SelectControl className={styles.control} value={draft.category} onChange={(event) => onEdit({ category: event.target.value })}>
              <option value="">{text("None")}</option>
              {categories.map((category) => <option key={category.name} value={category.name}>{category.i18n?.[locale] ?? category.label}</option>)}
            </SelectControl>
          </Field>
        ) : (
          <Notice tone={categoriesError ? "warning" : "info"}>
            {categoriesError
              ? text("Categories couldn't be loaded right now — you can continue and pick one later.")
              : text("No categories are available right now — you can continue without one.")}
          </Notice>
        )}
        {privateFieldsEditable && (
          <Field label={text("Details for moderators")}>
            <textarea className={styles.control} rows={3} maxLength={DESCRIPTION_MAX} value={draft.details} onChange={(event) => onEdit({ details: event.target.value })} />
          </Field>
        )}
      </div>
    </fieldset>
  );
}

function ScheduleFields({ draft, onEdit }: { draft: HangoutDraft; onEdit: EditFn }) {
  const { text } = useEventsCopy();
  return (
    <fieldset className={styles.editorSection}>
      <legend>{text("Schedule")}</legend>
      <div className={styles.stack}>
        <div className={styles.row}>
          <Field label={text("Date")} className={styles.grow}>
            <input className={styles.control} type="date" value={draft.startDate} onChange={(event) => onEdit({ startDate: event.target.value })} />
          </Field>
          <Field label={text("Start (UTC)")} className={styles.narrow}>
            <input className={styles.control} type="time" value={draft.startTime} onChange={(event) => onEdit({ startTime: event.target.value })} />
          </Field>
          <Field label={text("Duration (hours)")} className={styles.narrow}>
            <input className={styles.control} type="number" min="0.25" max="24" step="0.25" value={draft.durationHours} onChange={(event) => onEdit({ durationHours: Number(event.target.value) })} />
          </Field>
        </div>
        <div className={styles.row}>
          <Field label={text("Repeat hangout")} className={styles.grow}>
            <SelectControl
              className={styles.control}
              value={draft.recurrent ? draft.recurrence : ""}
              onChange={(event) => onEdit(event.target.value ? { recurrent: true, recurrence: event.target.value } : { recurrent: false })}
            >
              <option value="">{text("Does not repeat")}</option>
              {RECURRENCES.map(([value, label]) => <option key={value} value={value}>{text(label)}</option>)}
            </SelectControl>
          </Field>
          {draft.recurrent && (
            <Field label={text("Ends")} className={styles.grow}>
              <input className={styles.control} type="date" value={draft.recurrentUntil} onChange={(event) => onEdit({ recurrentUntil: event.target.value, recurrentCount: null })} />
            </Field>
          )}
        </div>
      </div>
    </fieldset>
  );
}

function LocationFields({ draft, options, onEdit }: { draft: HangoutDraft; options: HangoutOptions | null; onEdit: EditFn }) {
  const { text } = useEventsCopy();
  const isWorld = draft.location === "world";
  return (
    <fieldset className={styles.group}>
      <legend className={styles.groupTitle}>{text("Location")}</legend>
      <div className={styles.row}>
        <Field label={text("Location Type")} className={styles.grow}>
          <SelectControl className={styles.control} value={draft.location} onChange={(event) => onEdit({ location: event.target.value === "world" ? "world" : "land" })}>
            <option value="land">{text("Land")}</option>
            <option value="world">{text("World")}</option>
          </SelectControl>
        </Field>
        {isWorld ? (
          <Field
            label={text("World")}
            className={styles.grow}
            footer={options?.worldsError && <span className={styles.fieldHint}>{text("World suggestions could not load. You can enter the world name.")}</span>}
          >
            <input className={styles.control} type="text" list="hangout-worlds" value={draft.worldName} placeholder="my-world.dcl.eth" onChange={(event) => onEdit({ worldName: event.target.value })} />
            <datalist id="hangout-worlds">{options?.worlds.map((world) => <option value={world} key={world} />)}</datalist>
          </Field>
        ) : (
          <>
            <Field label={text("Latitude (X)")} className={styles.narrow}>
              <input className={styles.control} type="number" value={draft.coordX} onChange={(event) => onEdit({ coordX: Number(event.target.value) })} />
            </Field>
            <Field label={text("Longitude (Y)")} className={styles.narrow}>
              <input className={styles.control} type="number" value={draft.coordY} onChange={(event) => onEdit({ coordY: Number(event.target.value) })} />
            </Field>
          </>
        )}
      </div>
    </fieldset>
  );
}

/**
 * The editor's page while the event to edit is read, or when that read failed and can be retried:
 * the same header, so nothing jumps when the form arrives.
 */
export function SubmitHangoutLoading({ mode, error, onRetry, onSignIn }: { mode: "create" | "edit"; error?: string | null; onRetry?: () => void; onSignIn?: () => void }) {
  const { text, errorText } = useEventsCopy();
  return (
    <div className={`submit-hangout ${styles.authoring}`} data-editor>
      <EditorHeader mode={mode} />
      {error ? (
        <div className={styles.loadState}>
          <Notice tone="error">{errorText(error)}</Notice>
          {onRetry && <SitesButton variant="tertiary" onClick={onRetry}>{text("Retry")}</SitesButton>}
        </div>
      ) : onSignIn ? (
        <div className={styles.loadState}>
          <Notice>{text("Sign in to edit your hangout.")}</Notice>
          <SitesButton variant="primary" onClick={onSignIn}>{text("Sign in")}</SitesButton>
        </div>
      ) : <div className={styles.loadState}><Notice>{text("Loading your hangout…")}</Notice></div>}
    </div>
  );
}

function ForwardButton({
  from,
  enabled,
  send,
  emitTrack,
  trackCtx,
  nextStep,
}: {
  from: HangoutStateId;
  enabled: boolean;
  send: (e: { type: "NEXT" }) => void;
  emitTrack: TrackFn;
  trackCtx: TrackContext;
  nextStep?: HangoutStateId;
}) {
  const { text } = useEventsCopy();
  return (
    <button
      type="button"
      className="submit-hangout__btn submit-hangout__btn--primary"
      disabled={!enabled}
      onClick={() => {
        if (!enabled) return;
        send({ type: "NEXT" });
        if (nextStep) emitStepCompleted(emitTrack, trackCtx, from, nextStep);
      }}
    >
      {text("Continue")}
    </button>
  );
}

function StepFields({
  step,
  draft,
  categories,
  categoriesError,
  onEdit,
  onUpload,
  uploading,
  privateFieldsEditable,
  options,
}: {
  step: HangoutStateId;
  draft: HangoutDraft;
  categories: CategoryOption[];
  categoriesError: boolean;
  onEdit: (patch: Partial<HangoutDraft>) => void;
  onUpload?: (file: File, vertical: boolean) => void;
  uploading: boolean;
  privateFieldsEditable: boolean;
  options: HangoutOptions | null;
}) {
  const { locale, text } = useEventsCopy();
  const [verticalCoverOpen, setVerticalCoverOpen] = useState(Boolean(draft.verticalImagePreviewUrl));
  if (step === "cover") {
    return (
      <div className="submit-hangout__fields">
        <p className="submit-hangout__hint">{text("Cover images are optional.")}</p>
        <CoverField
          image={draft.imagePreviewUrl}
          uploading={uploading}
          onUpload={onUpload}
          onRemove={() => onEdit({ imagePreviewUrl: null })}
        />
        <details className={styles.verticalCover} open={verticalCoverOpen} onToggle={(event) => setVerticalCoverOpen(event.currentTarget.open)}>
          <summary>{text("Optional vertical cover")}</summary>
          <CoverField
            vertical
            image={draft.verticalImagePreviewUrl}
            uploading={uploading}
            onUpload={onUpload}
            onRemove={() => onEdit({ verticalImagePreviewUrl: null })}
          />
        </details>
      </div>
    );
  }

  if (step === "details") {
    return (
      <div className="submit-hangout__fields">
        <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
          <span>{text("Hangout Name")}</span>
          <input
            className={`submit-hangout__field ${styles.field}`}
            type="text"
            maxLength={HANGOUT_BOUNDS.nameMax}
            value={draft.name}
            placeholder={text("Be as descriptive as you can")}
            onChange={(e) => onEdit({ name: e.target.value })}
          />
        </label>
        <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
          <span>{text("Description")}</span>
          <textarea
            className={`submit-hangout__field ${styles.field}`}
            rows={2}
            maxLength={DESCRIPTION_MAX}
            value={draft.description}
            onChange={(e) => onEdit({ description: e.target.value })}
          />
        </label>
        {categories.length > 0 ? (
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
            {text("Category")}
            <select
              className={`submit-hangout__field ${styles.field}`}
              value={draft.category}
              onChange={(e) => onEdit({ category: e.target.value })}
            >
              <option value="">{text("None")}</option>
              {categories.map((c) => (
                <option key={c.name} value={c.name}>
                  {c.i18n?.[locale] ?? c.label}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <Notice tone={categoriesError ? "warning" : "info"}>
            {categoriesError
              ? text("Categories couldn't be loaded right now — you can continue and pick one later.")
              : text("No categories are available right now — you can continue without one.")}
          </Notice>
        )}
        {privateFieldsEditable && <>
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>{text("Contact email")}<input className={`submit-hangout__field ${styles.field}`} type="email" value={draft.contact} onChange={(event) => onEdit({ contact: event.target.value })} /></label>
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>{text("Details for moderators")}<textarea aria-label={text("Details for moderators")} className={`submit-hangout__field ${styles.field}`} value={draft.details} maxLength={DESCRIPTION_MAX} onChange={(event) => onEdit({ details: event.target.value })} /></label>
        </>}
      </div>
    );
  }

  if (step === "location") {
    const isWorld = draft.location === "world";
    return (
      <div className="submit-hangout__fields">
        <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
          {text("Location Type")}
          <select
            className={`submit-hangout__field ${styles.field}`}
            value={draft.location}
            onChange={(e) =>
              onEdit({ location: e.target.value === "world" ? "world" : "land" })
            }
          >
            <option value="land">{text("Land")}</option>
            <option value="world">{text("World")}</option>
          </select>
        </label>
        {isWorld ? (
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
            {text("World")}
            <input
              className={`submit-hangout__field ${styles.field}`}
              type="text"
              list="hangout-worlds"
              value={draft.worldName}
              placeholder="my-world.dcl.eth"
              onChange={(e) => onEdit({ worldName: e.target.value })}
            />
            <datalist id="hangout-worlds">{options?.worlds.map((world) => <option value={world} key={world} />)}</datalist>
            {options?.worldsError && <span className="submit-hangout__hint">{text("World suggestions could not load. You can enter the world name.")}</span>}
          </label>
        ) : (
          <div className="submit-hangout__coords">
            <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
              X
              <input
                className={`submit-hangout__field ${styles.field}`}
                type="number"
                value={draft.coordX}
                onChange={(e) => onEdit({ coordX: Number(e.target.value) })}
              />
            </label>
            <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
              Y
              <input
                className={`submit-hangout__field ${styles.field}`}
                type="number"
                value={draft.coordY}
                onChange={(e) => onEdit({ coordY: Number(e.target.value) })}
              />
            </label>
          </div>
        )}
      </div>
    );
  }

  if (step === "schedule") {
    return (
      <div className="submit-hangout__fields">
        <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
          {text("Date")}
          <input
            className={`submit-hangout__field ${styles.field}`}
            type="date"
            value={draft.startDate}
            onChange={(e) => onEdit({ startDate: e.target.value })}
          />
        </label>
        <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
          {text("Start (UTC)")}
          <input
            className={`submit-hangout__field ${styles.field}`}
            type="time"
            value={draft.startTime}
            onChange={(e) => onEdit({ startTime: e.target.value })}
          />
        </label>
        <label className="submit-hangout__fieldcheck">
          {text("Duration (hours)")}
          <input className={`submit-hangout__field ${styles.field}`} type="number" min="0.25" max="24" step="0.25" value={draft.durationHours} onChange={(event) => onEdit({ durationHours: Number(event.target.value) })} />
        </label>
        <label className="submit-hangout__fieldcheck">
          <input
            type="checkbox"
            checked={draft.recurrent}
            onChange={(e) => onEdit({ recurrent: e.target.checked })}
          />
          {text("Repeat Hangout")}
        </label>
        {draft.recurrent && (
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>{text("Repeat")}<select className={`submit-hangout__field ${styles.field}`} value={draft.recurrence} onChange={(event) => onEdit({ recurrence: event.target.value })}>{RECURRENCES.map(([option, label]) => <option key={option} value={option}>{text(label)}</option>)}</select></label>
        )}
        {draft.recurrent && (
          <label className={`submit-hangout__fieldlabel ${styles.fieldLabel}`}>
            {text("Ends")}
            <input
              className={`submit-hangout__field ${styles.field}`}
              type="date"
              value={draft.recurrentUntil}
              onChange={(e) => onEdit({ recurrentUntil: e.target.value, recurrentCount: null })}
            />
          </label>
        )}
      </div>
    );
  }

  return (
    <div className="submit-hangout__review">
      <h3 className="submit-hangout__reviewtitle">{text("Review your hangout")}</h3>
      <dl className="submit-hangout__reviewlist">
        <dt>{text("Name")}</dt>
        <dd>{draft.name || "\u{2014}"}</dd>
        <dt>{text("When")}</dt>
        <dd>
          {draft.startDate || "\u{2014}"} {draft.startTime}
          {draft.recurrent ? ` · ${text("repeats until {date}", { date: draft.recurrentUntil || "\u{2014}" })}` : ""}
        </dd>
        <dt>{text("Where")}</dt>
        <dd>
          {draft.location === "world"
            ? draft.worldName || text("a world")
            : `LAND (${draft.coordX}, ${draft.coordY})`}
        </dd>
        {draft.category && (
          <>
            <dt>{text("Category")}</dt>
            <dd>{categories.find(c => c.name === draft.category)?.i18n?.[locale] ?? draft.category}</dd>
          </>
        )}
      </dl>
    </div>
  );
}

function CoverField({
  inline = false,
  vertical = false,
  image,
  uploading,
  onUpload,
  onRemove,
}: {
  inline?: boolean;
  vertical?: boolean;
  image: string | null;
  uploading: boolean;
  onUpload?: (file: File, vertical: boolean) => void;
  onRemove: () => void;
}) {
  const { text } = useEventsCopy();
  const uploadLabel = text(vertical ? "Upload vertical cover" : "Upload cover image");
  function dropCover(event: DragEvent<HTMLElement>) {
    event.preventDefault();
    const file = event.dataTransfer.files[0];
    if (!uploading && file) onUpload?.(file, vertical);
  }
  return (
    <div className={styles.coverField}>
      {image && (
        <figure className={styles.cover} onDragOver={(event) => event.preventDefault()} onDrop={dropCover}>
          <img className={styles.coverImage} src={image} alt={text(vertical ? "Vertical hangout cover" : "Hangout cover")} width={vertical ? 120 : 240} height={vertical ? 180 : 120} />
          <SitesButton variant="tertiary" size="sm" disabled={uploading} onClick={onRemove}>
            {text(vertical ? "Remove vertical cover" : "Remove cover image")}
          </SitesButton>
        </figure>
      )}
      {onUpload && (!inline || !image) && (
        <label
          className={styles.dropZone}
          onDragOver={(event) => event.preventDefault()}
          onDrop={dropCover}
        >
          {inline ? (
            <>
              <GeneralGallery className={styles.gallery} size={48} aria-hidden="true" />
              <strong>{vertical ? uploadLabel : text("Select an event cover")}</strong>
              <span>{text("Choose a picture from your gallery or drop it here")}</span>
              {!vertical && <small>{text("(recommended size: 1340 × 670)")}</small>}
            </>
          ) : (
            <>
              <strong>{uploadLabel}</strong>
              <span>{text("Drop an image here or choose a file, up to 500 KiB.")}</span>
            </>
          )}
          <input
            type="file"
            aria-label={uploadLabel}
            accept={vertical ? "image/jpeg,image/png,image/webp" : "image/jpeg,image/png,image/webp,image/gif"}
            disabled={uploading}
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              event.currentTarget.value = "";
              if (file) onUpload(file, vertical);
            }}
          />
        </label>
      )}
    </div>
  );
}
