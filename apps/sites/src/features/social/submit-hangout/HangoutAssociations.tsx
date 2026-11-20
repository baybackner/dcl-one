import { useEventsCopy } from "@dcl/one-ui/apps/sites/events/locale";
import { Notice } from "@dcl/one-ui/apps/sites/events/Notice";
import type { HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import type { HangoutOptions } from "@/data/lib/catalyst/landings/hangout-options";
import styles from "./HangoutAssociations.module.css";
import form from "./SubmitHangout.module.css";
import FeaturedAssetPicker from "./FeaturedAssetPicker";
import SelectControl from "./SelectControl";

type Edit = (patch: Partial<HangoutDraft>) => void;

/** The community the event is published for, with the read state of the account's communities. */
export function CommunityField({ draft, options, canChangeCommunity, onEdit, className }: {
  draft: HangoutDraft; options: HangoutOptions | null; canChangeCommunity: boolean; onEdit: Edit; className?: string;
}) {
  const { text } = useEventsCopy();
  const knownCommunity = options?.communities.some((community) => community.id === draft.communityId);
  return <div className={[styles.community, className].filter(Boolean).join(" ")}>
    <label className={`submit-hangout__fieldlabel ${form.fieldLabel} ${styles.columnLabel}`}><span className={form.fieldTitle}>{text("Community")}</span>
      <SelectControl aria-label={text("Community")} className={`submit-hangout__field ${form.field}`} value={draft.communityId} disabled={!canChangeCommunity} onChange={(event) => onEdit({ communityId: event.target.value })}>
        <option value="">{text("None")}</option>
        {draft.communityId && !knownCommunity && <option value={draft.communityId}>{text("Current community")}</option>}
        {options?.communities.map((community) => <option value={community.id} key={community.id}>{community.name}</option>)}
      </SelectControl>
    </label>
    {!options && <p role="status" className={form.fieldHint}>{text("Loading communities and schedules…")}</p>}
    {options?.communitiesError && <Notice tone="error">{text("Could not load your communities: {error}", { error: options.communitiesError })}</Notice>}
    {!canChangeCommunity && draft.communityId && <p className={`submit-hangout__hint ${form.fieldHint}`}>{text("Community owners and moderators can change this association.")}</p>}
  </div>;
}

/** Controls only an account that may edit any event sees: schedule associations and the join link. */
export function AdminFields({ draft, options, onEdit, className }: {
  draft: HangoutDraft; options: HangoutOptions | null; onEdit: Edit; className?: string;
}) {
  const { text } = useEventsCopy();
  const schedules = options?.schedules ?? [];
  const selectedSchedules = draft.scheduleIds ?? [];
  return <div className={[styles.admin, className].filter(Boolean).join(" ")}>
    <fieldset className={styles.schedules}><legend>{text("Schedules")}</legend>
      {options?.schedulesError ? <Notice tone="error">{text("Could not load schedules: {error}", { error: options.schedulesError })}</Notice> : <>
        {schedules.length === 0 && options && <p className="submit-hangout__hint">{text("No schedules are available.")}</p>}
        {schedules.map((schedule) => <label className="submit-hangout__fieldcheck" key={schedule.id}><input type="checkbox" checked={selectedSchedules.includes(schedule.id)} onChange={(event) => onEdit({ scheduleIds: event.target.checked ? [...selectedSchedules, schedule.id] : selectedSchedules.filter((id) => id !== schedule.id) })} />{schedule.name}</label>)}
        {selectedSchedules.filter((id) => !schedules.some((schedule) => schedule.id === id)).map((id) => <label className="submit-hangout__fieldcheck" key={id}><input type="checkbox" checked onChange={() => onEdit({ scheduleIds: selectedSchedules.filter((selected) => selected !== id) })} />{text("Unavailable schedule (remove association)")}</label>)}
      </>}
    </fieldset>
    <label className={`submit-hangout__fieldlabel ${form.fieldLabel} ${styles.columnLabel}`}><span className={form.fieldTitle}>{text("Custom join link")}</span><input type="url" className={`submit-hangout__field ${form.field}`} value={draft.joinUrl ?? ""} onChange={(event) => onEdit({ joinUrl: event.target.value })} placeholder={text("Use the venue's default link")} /></label>
  </div>;
}

/** The step wizard's associations: community, featured asset and, for event editors, the admin controls. */
export default function HangoutAssociations({ draft, options, canEditAny, canChangeCommunity, onEdit }: {
  draft: HangoutDraft; options: HangoutOptions | null; canEditAny: boolean; canChangeCommunity: boolean;
  onEdit: Edit;
}) {
  return <div className={styles.root}>
    <CommunityField draft={draft} options={options} canChangeCommunity={canChangeCommunity} onEdit={onEdit} />
    <FeaturedAssetPicker value={draft.featuredItem ?? ""} onChange={(featuredItem) => onEdit({ featuredItem })} />
    {canEditAny && <AdminFields draft={draft} options={options} onEdit={onEdit} />}
  </div>;
}
