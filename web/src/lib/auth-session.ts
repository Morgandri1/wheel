import { ApiError } from "@/lib/auth";

/**
 * The paste-code sign-in has a deadline, and the failures that follow it are indistinguishable
 * from "you typed the code wrong" unless we say which is which. This module holds that judgement
 * so the panel can stay about rendering.
 */

/** `expires_in` is seconds from now; an absent one means the engine did not commit to a deadline. */
export function expiryFrom(expiresIn: number | null | undefined, now: number): number | null {
  if (typeof expiresIn !== "number" || !Number.isFinite(expiresIn) || expiresIn <= 0) return null;
  return now + expiresIn * 1000;
}

/** `m:ss` remaining, or null once there is nothing left to count. */
export function countdown(expiresAt: number | null, now: number): string | null {
  if (expiresAt === null) return null;
  const left = Math.max(0, expiresAt - now);
  if (left === 0) return null;
  const total = Math.ceil(left / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

export type CompleteFailure = {
  /** `expired` is the only one where the fix is to start over rather than retype. */
  kind: "expired" | "rejected" | "other";
  message: string;
};

/**
 * 409 means the engine no longer holds the session this code belongs to — the code may be
 * perfect and still not work, so offering "try again" without restarting would loop someone
 * forever. Everything else keeps the engine's own words: it knows why it said no.
 */
export function completeFailure(e: unknown): CompleteFailure {
  if (e instanceof ApiError) {
    if (e.status === 409) {
      return {
        kind: "expired",
        message: "That sign-in window closed before the code arrived. Start it again — the new code is different.",
      };
    }
    if (e.status === 400) return { kind: "rejected", message: e.message };
    // Observed against the live API: submitting a wrong code makes the engine sit on the
    // exchange until the gateway gives up, so the operator gets a timeout for what is almost
    // always a typo. We cannot claim to know which it was, so the wording covers both without
    // asserting either — and does not leave them staring at "the engine did not respond".
    if (e.status === 502 || e.status === 503 || e.status === 504) {
      return {
        kind: "other",
        message:
          "The engine did not answer in time. That usually means the code was wrong or the window closed — start the sign-in again. If it keeps happening, the project may need a restart.",
      };
    }
    return { kind: "other", message: e.message };
  }
  return { kind: "other", message: "The engine could not be reached to finish signing in." };
}

/**
 * What a credential saved into a vault actually buys, in the engine's own terms.
 *
 * A paste-code login yields a SHORT-LIVED access token, so sharing it through a vault is a
 * convenience with an expiry date, not the durable path (that is one login per agent). The
 * engine sends `expires_at`/`warning`; if it says nothing we still say the honest generic
 * rather than let silence read as "permanent".
 */
export function vaultShareNote(
  saved: { expires_at?: string | null; warning?: string | null } | null | undefined,
): string | null {
  if (!saved) return null;
  if (saved.warning) return saved.warning;
  if (saved.expires_at) {
    const when = new Date(saved.expires_at);
    if (!Number.isNaN(when.getTime())) {
      return `Shared through the vault until ${when.toLocaleString()}. This is a short-lived token, not a permanent login.`;
    }
  }
  return "Shared through the vault. This is a short-lived token, not a permanent login.";
}
