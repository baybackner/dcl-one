import { adminCopy } from "@dcl/one-ui/apps/sites/events/admin-copy";
import { AdminPage, adminPageStyles } from "@dcl/one-ui/apps/sites/events/AdminPage";
import { PermissionsModal } from "@dcl/one-ui/apps/sites/events/PermissionsModal";
import { UsersTable, type AdminUserRow } from "@dcl/one-ui/apps/sites/events/UsersTable";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { getIdentity } from "@/data/lib/auth/session";
import { eventAdminKeys, saveEventPermissions, type EventProfile } from "@/data/lib/catalyst/places/event-management";
import { creatorProfilesQuery } from "@/data/lib/catalyst/social/creator-profiles";
import { useEventsFooterCopy } from "../../events/locale";
import { openSignIn } from "../../components/auth/signin-store";
import SitesLayout from "../../components/chrome/SitesLayout";
import { AdminAccessGate } from "./AdminAccessGate";
import { EventAdminTabs } from "./EventAdminTabs";
import { useEventAdmin, type EventAdminAccess } from "./useEventAdmin";
import { useToast } from "./useToast";

type Editing = { user?: AdminUserRow; permissions: readonly string[] };

export default function UsersAdminPage() {
  const access = useEventAdmin();
  const footerCopy = useEventsFooterCopy();
  return (
    <SitesLayout active="whatson" headerSurface="overlay" overlayNav footerCopy={footerCopy}>
      <AdminPage bar={<EventAdminTabs active="users" />}>
        <AdminAccessGate access={access} allowed={access.canManageUsers}>
          <UsersAdmin key={access.identity?.signer.toLowerCase()} access={access} profiles={access.profiles ?? []} />
        </AdminAccessGate>
      </AdminPage>
    </SitesLayout>
  );
}

function UsersAdmin({ access, profiles }: { access: EventAdminAccess; profiles: EventProfile[] }) {
  const client = useQueryClient();
  const names = useQuery({ ...creatorProfilesQuery(profiles.map((profile) => profile.user)), enabled: profiles.length > 0 });
  const [editing, setEditing] = useState<Editing | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useToast();
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);

  const rows: AdminUserRow[] = profiles.map((profile) => {
    const found = names.data?.[profile.user.toLowerCase()];
    return { address: profile.user, name: found?.name || undefined, avatarUrl: found?.avatarUrl, permissions: profile.permissions };
  });

  function open(next: Editing) {
    setError(null);
    setEditing(next);
  }

  async function save({ address, permissions }: { address: string; permissions: string[] }) {
    const signer = getIdentity();
    if (!signer || signer.signer.toLowerCase() !== access.identity?.signer.toLowerCase()) {
      openSignIn();
      return;
    }
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await saveEventPermissions(signer, address, permissions);
      void client.invalidateQueries({ queryKey: eventAdminKeys.access(signer.signer) });
      if (!mounted.current) return;
      const kept = saved.permissions.length !== permissions.length || permissions.some((permission) => !saved.permissions.includes(permission));
      setEditing(null);
      setToast({ message: kept ? adminCopy.permissions.serverKept : adminCopy.permissions.saveSuccess, tone: "success" });
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error && cause.message ? cause.message : adminCopy.permissions.saveError);
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  return (
    <>
      <h1 className={adminPageStyles.title}>{adminCopy.users.title}</h1>
      <UsersTable rows={rows} onAdd={() => open({ permissions: [] })} onEdit={(row) => open({ user: row, permissions: row.permissions })} />
      {editing ? (
        <PermissionsModal
          key={editing.user?.address ?? "add"}
          user={editing.user}
          permissions={editing.permissions}
          self={access.identity?.signer}
          busy={busy}
          feedback={error ? <p className={adminPageStyles.error} role="alert">{error}</p> : undefined}
          onClose={() => { setError(null); setEditing(null); }}
          onSubmit={(payload) => void save(payload)}
        />
      ) : null}
      <div role="status">
        {toast ? <p className={adminPageStyles.toast} data-tone={toast.tone}>{toast.message}</p> : null}
      </div>
    </>
  );
}
