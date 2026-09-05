"use client";

import { useState } from "react";
import { Button, Field, Input } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { VaultNode } from "@/lib/schema";

/**
 * Vault values are write-only, and this panel is the proof rather than the promise: there is no
 * getter in the API client to call, so nothing here can render a secret even by mistake. What
 * comes back from the board is a list of KEY NAMES; the values only ever travel inwards.
 */
export function VaultPanel({
  node,
  api,
  onChanged,
}: {
  node: VaultNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);

  const keys = node.config.keys ?? [];
  const keyError =
    key && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)
      ? "Keys are letters, digits and underscore, starting with a letter."
      : null;
  const replacing = keys.includes(key);

  const save = async () => {
    setSaving(true);
    try {
      await api.putSecret(node.id, key, value);
      // Clear immediately: the value has left, and there is no reason for it to sit in a form.
      setKey("");
      setValue("");
      onChanged();
      toast(replacing ? `Replaced ${key}.` : `Stored ${key}.`);
    } catch (e) {
      toastError(e, "Couldn't store that secret.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <p className="text-meta text-ink-dim">
        Agents with a read wire get these as environment variables at spawn, and can fetch one
        with <span className="ident">wheel secret get</span>. You can set a value; nobody — not
        the board, not this panel, not the API — can read one back.
      </p>

      <div>
        <p className="mb-1.5 text-micro font-medium text-ink-dim">
          Keys ({keys.length})
        </p>
        {keys.length ? (
          <ul className="border border-rule" data-testid="vault-keys">
            {keys.map((k) => (
              <li
                key={k}
                data-testid={`vault-key-${k}`}
                className="flex items-center justify-between gap-2 border-b border-rule px-2.5 py-1.5 last:border-b-0"
              >
                <span className="ident text-micro text-ink">{k}</span>
                <span className="text-micro text-ink-faint">set</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-micro text-ink-faint" data-testid="vault-empty">
            No secrets yet. Whatever you add here is available to wired agents at their next start.
          </p>
        )}
      </div>

      <form
        className="flex flex-col gap-3 border-t border-rule pt-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (key && value && !keyError) void save();
        }}
      >
        <Field label="Key" error={keyError}>
          <Input
            mono
            data-testid="inspector-vault-key"
            value={key}
            placeholder="STRIPE_API_KEY"
            onChange={(e) => setKey(e.target.value)}
          />
        </Field>
        <Field
          label="Value"
          hint={
            replacing
              ? `Replaces the value already stored under ${key}.`
              : "Sent once, encrypted at rest, never returned."
          }
        >
          <Input
            type="password"
            mono
            autoComplete="off"
            data-testid="inspector-vault-value"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
        </Field>
        <div className="flex justify-end">
          <Button
            type="submit"
            tone="primary"
            size="sm"
            data-testid="btn-vault-save"
            disabled={!key || !value || Boolean(keyError) || saving}
          >
            {saving ? "Storing…" : replacing ? "Replace secret" : "Store secret"}
          </Button>
        </div>
      </form>
    </>
  );
}
