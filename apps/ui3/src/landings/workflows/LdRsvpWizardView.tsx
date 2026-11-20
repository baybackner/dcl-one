import { useEffect, useRef, useState } from "react";
import Button from "../../atoms/Button";
import EventDetail from "../../explorer/pages/EventDetail";
import "./ldrsvpwizardview.css";

type LdRsvpEventInfo = {
  title: string;
  when: string;
  host: string;
  description: string;
  schedule: string;
  location: string;
  jumpHref: string;
};

const EMPTY_EVENT: LdRsvpEventInfo = {
  title: "",
  when: "",
  host: "",
  description: "",
  schedule: "",
  location: "",
  jumpHref: "",
};

type LdRsvpWizardViewProps = {
  step?: string;
  value?: string;
  event?: LdRsvpEventInfo;
  count?: number;
  error?: string;
  onTapGoing?: () => void;
  onCancel?: () => void;
  onSignIn?: () => void;
  onBack?: () => void;
  onConfirm?: () => void;
  onCancelRsvp?: () => void;
  onDismiss?: () => void;
  onRetry?: () => void;
  /** Whether the event detail covers the panel; uncontrolled (open at first) when omitted. */
  detailOpen?: boolean;
  onDetailOpenChange?: (open: boolean) => void;
};

export default function LdRsvpWizardView({
  step = "idle",
  value = "idle",
  event = EMPTY_EVENT,
  count = 0,
  error = undefined,
  onTapGoing = undefined,
  onCancel = undefined,
  onSignIn = undefined,
  onBack = undefined,
  onConfirm = undefined,
  onCancelRsvp = undefined,
  onDismiss = undefined,
  onRetry = undefined,
  detailOpen: controlledOpen = undefined,
  onDetailOpenChange = undefined,
}: LdRsvpWizardViewProps) {
  // The event detail opens over the RSVP panel; Close, the backdrop or Escape dismisses it so the
  // panel's actions can be reached, and "Event details" brings it back.
  const [ownOpen, setOwnOpen] = useState(true);
  const detailOpen = controlledOpen ?? ownOpen;
  const panel = useRef<HTMLDivElement>(null);
  const setDetailOpen = (open: boolean) => {
    setOwnOpen(open);
    onDetailOpenChange?.(open);
  };
  const closeDetail = () => {
    setDetailOpen(false);
    panel.current?.focus();
  };
  useEffect(() => {
    if (!detailOpen) return undefined;
    const onKeyDown = (key: KeyboardEvent) => {
      if (key.key !== "Escape" || key.defaultPrevented) return;
      setOwnOpen(false);
      onDetailOpenChange?.(false);
      panel.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [detailOpen, onDetailOpenChange]);

  return (
    <div className="rsvp-wizard" data-step={step}>
      {detailOpen ? (
        <EventDetail
          event={{
            title: event.title,
            when: event.when,
            host: event.host,
            description: event.description,
            schedule: event.schedule,
            location: event.location,
          }}
          jumpHref={event.jumpHref}
          onClose={closeDetail}
        />
      ) : null}

      <div
        ref={panel}
        tabIndex={-1}
        className="rsvp-wizard__panel"
        role="group"
        aria-label="RSVP to this event"
      >
        <div className="rsvp-wizard__count" aria-live="polite">
          <strong>{count}</strong> going
        </div>

        {value === "idle" && (
          <Button onClick={onTapGoing}>Going</Button>
        )}

        {value === "signinGate" && (
          <>
            <p className="rsvp-wizard__hint">
              Sign in to RSVP. Your wallet signs the request.
            </p>
            <div className="rsvp-wizard__row">
              <Button variant="secondary" onClick={onCancel}>
                Cancel
              </Button>
              <Button onClick={onSignIn}>Sign in</Button>
            </div>
          </>
        )}

        {value === "confirming" && (
          <>
            <p className="rsvp-wizard__hint">
              Confirm you&apos;re going to <b>{event.title}</b>. You can cancel
              your RSVP anytime.
            </p>
            <div className="rsvp-wizard__row">
              <Button variant="secondary" onClick={onBack}>
                Back
              </Button>
              <Button onClick={onConfirm}>Confirm RSVP</Button>
            </div>
          </>
        )}

        {value === "submitting" && (
          <p className="rsvp-wizard__hint" aria-busy="true">
            Submitting your RSVP&#x2026;
          </p>
        )}

        {value === "going" && (
          <>
            <p className="rsvp-wizard__hint rsvp-wizard__hint--ok">
              You&apos;re going! See you there.
            </p>
            <Button variant="secondary" onClick={onCancelRsvp}>
              Cancel RSVP
            </Button>
          </>
        )}

        {value === "cancelling" && (
          <p className="rsvp-wizard__hint" aria-busy="true">
            Cancelling your RSVP&#x2026;
          </p>
        )}

        {value === "notGoing" && (
          <>
            <p className="rsvp-wizard__hint">Your RSVP was cancelled.</p>
            <Button onClick={onTapGoing}>Going again</Button>
          </>
        )}

        {value === "error" && (
          <>
            <p className="rsvp-wizard__hint rsvp-wizard__hint--err" role="alert">
              RSVP failed{error ? `: ${error}` : ""}.
            </p>
            <div className="rsvp-wizard__row">
              <Button variant="secondary" onClick={onDismiss}>
                Dismiss
              </Button>
              <Button onClick={onRetry}>Retry</Button>
            </div>
          </>
        )}

        {!detailOpen && (
          <Button variant="ghost" onClick={() => setDetailOpen(true)}>
            Event details
          </Button>
        )}
      </div>
    </div>
  );
}
