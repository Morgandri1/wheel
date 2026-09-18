"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invites, members } from "@/lib/api";
import { TIERS, tierAtLeast, tierLabel, type Tier } from "@/lib/tiers";
import { Button, CopyField, Dialog, Field, Input, Select } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";

/**
 * `POST /v1/projects/{id}/members` upserts on `(project, user_id)`, so this one call both adds a
 * new member and changes an existing one's tier — there is no separate "change tier" route.
 *
 * This decides nothing: every control here is a courtesy for a caller who can already use it,
 * never the reason they can. The API enforces every tier per route, default-deny, so a non-admin
 * simply never gets the mutation controls rendered — hiding them saves a confusing 403, it does
 * not create the boundary.
 */
export function MembersPanel({
  open,
  onClose,
  projectId,
  tier,
}: {
  open: boolean;
  onClose: () => void;
  projectId: string;
  tier: Tier | undefined;
}) {
  const qc = useQueryClient();
  const isAdmin = tierAtLeast(tier, "admin");

  const roster = useQuery({
    queryKey: ["members", projectId],
    queryFn: () => members.list(projectId),
    enabled: open,
  });
  const pendingInvites = useQuery({
    queryKey: ["invites", projectId],
    queryFn: () => invites.list(projectId),
    enabled: open && isAdmin,
  });

  const invalidateMembers = () => qc.invalidateQueries({ queryKey: ["members", projectId] });
  const invalidateInvites = () => qc.invalidateQueries({ queryKey: ["invites", projectId] });

  const grant = useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: Tier }) =>
      members.grant(projectId, userId, role),
    onSuccess: invalidateMembers,
    onError: (e) => toastError(e, "Couldn't grant that membership."),
  });
  const revoke = useMutation({
    mutationFn: (userId: string) => members.revoke(projectId, userId),
    onSuccess: () => {
      invalidateMembers();
      toast("Removed from the project.");
    },
    onError: (e) => toastError(e, "Couldn't remove that member."),
  });

  const [addUserId, setAddUserId] = useState("");
  const [addRole, setAddRole] = useState<Tier>("prompter");
  const addMember = (e: React.FormEvent) => {
    e.preventDefault();
    const userId = addUserId.trim();
    if (!userId) return;
    grant.mutate(
      { userId, role: addRole },
      { onSuccess: () => setAddUserId("") },
    );
  };

  const [inviteRole, setInviteRole] = useState<Tier>("prompter");
  const [inviteEmail, setInviteEmail] = useState("");
  const [issued, setIssued] = useState<{ token: string; role: Tier } | null>(null);
  const createInvite = useMutation({
    mutationFn: () =>
      invites.create(projectId, {
        role: inviteRole,
        ...(inviteEmail.trim() ? { email: inviteEmail.trim() } : {}),
      }),
    onSuccess: (created) => {
      setIssued({ token: created.token, role: created.role });
      setInviteEmail("");
      invalidateInvites();
    },
    onError: (e) => toastError(e, "Couldn't create that invite."),
  });
  const revokeInvite = useMutation({
    mutationFn: (inviteId: string) => invites.revoke(projectId, inviteId),
    onSuccess: () => {
      invalidateInvites();
      toast("Invite revoked.");
    },
    onError: (e) => toastError(e, "Couldn't revoke that invite."),
  });

  return (
    <Dialog open={open} onClose={onClose} title="Members" testId="dialog-members">
      <div className="flex max-h-[70vh] flex-col gap-5 overflow-y-auto">
        <section className="flex flex-col gap-2">
          {roster.isPending ? (
            <p className="text-micro text-ink-faint">Loading…</p>
          ) : roster.error ? (
            <p className="text-micro text-[var(--danger)]">Could not load the roster.</p>
          ) : (
            <ul className="flex flex-col gap-1.5" data-testid="member-list">
              {roster.data ? (
                <MemberRow
                  userId={roster.data.creator}
                  email={roster.data.creator_email}
                  role="admin"
                  isCreator
                  isAdmin={isAdmin}
                />
              ) : null}
              {roster.data?.members.map((m) => (
                <MemberRow
                  key={m.user_id}
                  userId={m.user_id}
                  email={m.email}
                  role={m.role}
                  isAdmin={isAdmin}
                  onChangeRole={(role) => grant.mutate({ userId: m.user_id, role })}
                  onRemove={() => revoke.mutate(m.user_id)}
                  busy={grant.isPending || revoke.isPending}
                />
              ))}
            </ul>
          )}
        </section>

        {isAdmin ? (
          <>
            <section className="flex flex-col gap-2 border-t border-rule pt-4">
              <h3 className="text-meta font-medium text-ink">Add by account id</h3>
              <form method="post" className="flex items-end gap-2" onSubmit={addMember} data-testid="form-add-member">
                <div className="flex-1">
                  <Field label="User id">
                    <Input
                      value={addUserId}
                      onChange={(e) => setAddUserId(e.target.value)}
                      placeholder="from their GET /v1/auth/me"
                      data-testid="input-add-user-id"
                    />
                  </Field>
                </div>
                <Select
                  value={addRole}
                  onChange={(e) => setAddRole(e.target.value as Tier)}
                  data-testid="select-add-tier"
                >
                  {TIERS.map((t) => (
                    <option key={t} value={t}>
                      {tierLabel(t)}
                    </option>
                  ))}
                </Select>
                <Button
                  type="submit"
                  size="sm"
                  disabled={!addUserId.trim() || grant.isPending}
                  data-testid="btn-add-member"
                >
                  {grant.isPending ? "Adding…" : "Add"}
                </Button>
              </form>
            </section>

            <section className="flex flex-col gap-2 border-t border-rule pt-4">
              <h3 className="text-meta font-medium text-ink">Invite</h3>
              <div className="flex items-end gap-2">
                <div className="flex-1">
                  <Field label="Email (optional)">
                    <Input
                      type="email"
                      value={inviteEmail}
                      onChange={(e) => setInviteEmail(e.target.value)}
                      placeholder="locks the invite to that address"
                      data-testid="input-invite-email"
                    />
                  </Field>
                </div>
                <Select
                  value={inviteRole}
                  onChange={(e) => setInviteRole(e.target.value as Tier)}
                  data-testid="select-invite-tier"
                >
                  {TIERS.map((t) => (
                    <option key={t} value={t}>
                      {tierLabel(t)}
                    </option>
                  ))}
                </Select>
                <Button
                  size="sm"
                  disabled={createInvite.isPending}
                  onClick={() => createInvite.mutate()}
                  data-testid="btn-create-invite"
                >
                  {createInvite.isPending ? "Creating…" : "Create invite"}
                </Button>
              </div>
              {issued ? (
                <div className="rounded-control border border-rule bg-[var(--panel-0)] p-2.5">
                  <p className="mb-1.5 text-micro text-ink-dim">
                    {tierLabel(issued.role)} invite — shown once, copy it now:
                  </p>
                  <CopyField value={issued.token} testId="invite-token" />
                </div>
              ) : null}

              {pendingInvites.data?.length ? (
                <ul className="mt-1 flex flex-col gap-1" data-testid="invite-list">
                  {pendingInvites.data.map((inv) => (
                    <li
                      key={inv.id}
                      data-testid={`invite-${inv.id}`}
                      className="flex items-center gap-2 rounded-control border border-rule px-2.5 py-1.5 text-micro"
                    >
                      <span className="ident text-ink-dim">{tierLabel(inv.role)}</span>
                      <span className="text-ink-faint">{inv.email ?? "any account"}</span>
                      <span className="flex-1" />
                      <span className="text-ink-faint">
                        {inv.uses}/{inv.max_uses} used
                      </span>
                      <Button
                        size="sm"
                        tone="ghost"
                        disabled={revokeInvite.isPending}
                        onClick={() => revokeInvite.mutate(inv.id)}
                        data-testid={`btn-revoke-invite-${inv.id}`}
                      >
                        Revoke
                      </Button>
                    </li>
                  ))}
                </ul>
              ) : null}
            </section>
          </>
        ) : null}
      </div>
    </Dialog>
  );
}

function MemberRow({
  userId,
  email,
  role,
  isCreator,
  isAdmin,
  onChangeRole,
  onRemove,
  busy,
}: {
  userId: string;
  email?: string;
  role: Tier;
  isCreator?: boolean;
  isAdmin: boolean;
  onChangeRole?: (role: Tier) => void;
  onRemove?: () => void;
  busy?: boolean;
}) {
  return (
    <li
      data-testid={isCreator ? "member-creator" : `member-${userId}`}
      className="flex items-center gap-2 rounded-control border border-rule px-2.5 py-1.5 text-micro"
    >
      <span className="min-w-0 flex-1 truncate">
        <span className="text-ink">{email ?? userId}</span>
        {email ? <span className="ml-1.5 text-ink-faint">{userId}</span> : null}
      </span>
      {isCreator ? (
        <span className="ident text-ink-faint">Creator</span>
      ) : isAdmin ? (
        <Select
          value={role}
          disabled={busy}
          onChange={(e) => onChangeRole?.(e.target.value as Tier)}
          data-testid={`select-tier-${userId}`}
        >
          {TIERS.map((t) => (
            <option key={t} value={t}>
              {tierLabel(t)}
            </option>
          ))}
        </Select>
      ) : (
        <span className="ident text-ink-faint">{tierLabel(role)}</span>
      )}
      {!isCreator && isAdmin ? (
        <Button
          size="sm"
          tone="ghost"
          disabled={busy}
          onClick={onRemove}
          data-testid={`btn-remove-${userId}`}
        >
          Remove
        </Button>
      ) : null}
    </li>
  );
}
