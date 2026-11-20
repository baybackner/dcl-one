import { Dialog } from "@dcl/one-ui/apps/sites/events/Dialog";
import { useEventsCopy } from "@dcl/one-ui/apps/sites/events/locale";
import { Button } from "@dcl/one-ui/apps/sites/shared/Button/Button";
import { useId } from "react";

import styles from "./DeleteEventDialog.module.css";

/** Confirms deleting an event; it cannot be dismissed while the delete is in flight. */
export default function DeleteEventDialog({ busy, error, onCancel, onConfirm }: {
  busy: boolean;
  error: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { text } = useEventsCopy();
  const titleId = useId();
  const cancel = () => { if (!busy) onCancel(); };
  return (
    <Dialog labelledBy={titleId} onClose={cancel} maxWidth={540}>
      <div className={styles.body}>
        <h2 id={titleId} className={styles.title}>{text("Are you sure you want to delete your event?")}</h2>
        <p className={styles.copy}>{text("This event will be permanently removed from your events. This action can’t be undone.")}</p>
        {error && <p className={styles.error} role="alert">{text("We couldn't delete your event. Please try again.")}</p>}
        <div className={styles.actions}>
          <Button variant="inverse" disabled={busy} onClick={cancel}>{text("Cancel")}</Button>
          <Button disabled={busy} aria-busy={busy || undefined} onClick={onConfirm}>{busy ? text("Deleting…") : text("Delete event")}</Button>
        </div>
      </div>
    </Dialog>
  );
}
