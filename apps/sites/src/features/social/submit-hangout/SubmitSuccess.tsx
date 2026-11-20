import { useEventsCopy } from "@dcl/one-ui/apps/sites/events/locale";
import { Button } from "@dcl/one-ui/apps/sites/shared/Button/Button";
import { GeneralCheck } from "@dcl/one-ui/foundations/icons";
import { useEffect, useRef } from "react";
import { useNavigate } from "react-router";

import styles from "./SubmitSuccess.module.css";

/**
 * The confirmation that covers the page below the site header once an event is created or saved.
 * The form it replaces held focus, so focus moves to the message instead of falling to the page.
 */
export default function SubmitSuccess({ mode, approved }: { mode: "create" | "edit"; approved: boolean }) {
  const { text } = useEventsCopy();
  const navigate = useNavigate();
  const message = useRef<HTMLParagraphElement | null>(null);
  useEffect(() => {
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    message.current?.focus();
    return () => { document.body.style.overflow = overflow; };
  }, []);
  return (
    <section className={styles.overlay}>
      <div className={styles.content}>
        <span className={styles.check}><GeneralCheck size={56} aria-hidden="true" /></span>
        <p ref={message} className={styles.message} role="status" tabIndex={-1}>
          {mode === "edit"
            ? text("Your event was updated successfully.")
            : approved
              ? text("Your event was created and is now live.")
              : text("Your event was created successfully. You'll be notified once it's approved. Track its status in \"My Events\"")}
        </p>
        <div className={styles.actions}>
          <Button variant="inverse" className={styles.action} onClick={() => { void navigate("/events"); }}>{text("Back to Explore")}</Button>
          <Button className={styles.action} onClick={() => { void navigate("/events?tab=my"); }}>{text("My Events")}</Button>
        </div>
      </div>
    </section>
  );
}
