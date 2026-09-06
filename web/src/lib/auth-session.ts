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
    // A wrong code is a 400 now (SDK fb74bfd, verified in production: 400 in 1.1s with
    // "Invalid code. Please make sure the full code was copied."). So a 5xx here no longer means
    // "probably a typo" — it means the engine genuinely did not answer, and saying otherwise
    // would send someone hunting for a mistake they did not make.
    if (e.status === 502 || e.status === 503 || e.status === 504) {
      return {
        kind: "other",
        message:
          "The engine did not answer, so this sign-in could not be finished. The code is fine — try again in a moment, and if it keeps happening the project needs a restart.",
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

/**
 * When a stored credential stops working, said before it does rather than after.
 *
 * `null`/absent means durable OR unknown, and the engine deliberately will not guess between
 * them — so neither does this. Saying nothing is the only honest option: inventing "expires
 * soon" for a durable key is a false alarm, and inventing "durable" for a session token is the
 * failure this field exists to prevent.
 *
 * A credential shared into a vault expires for every agent reading that vault at the same moment,
 * which is why this is worth surfacing rather than leaving to a support round-trip.
 */
export function expiryMessage(
  expiresAt: string | null | undefined,
  now: number,
): { text: string; urgent: boolean } | null {
  if (!expiresAt) return null;
  const when = new Date(expiresAt);
  const at = when.getTime();
  if (Number.isNaN(at)) return null;

  const msLeft = at - now;
  if (msLeft <= 0) return { text: "These credentials have expired — sign in again.", urgent: true };

  const hours = msLeft / 3_600_000;
  if (hours < 1) {
    const minutes = Math.max(1, Math.round(msLeft / 60_000));
    return { text: `Expires in ${minutes} minute${minutes === 1 ? "" : "s"} — sign in again soon.`, urgent: true };
  }
  if (hours < 24) {
    const whole = Math.round(hours);
    return { text: `Expires in ${whole} hour${whole === 1 ? "" : "s"}.`, urgent: whole <= 6 };
  }
  return { text: `Expires ${when.toLocaleString()}.`, urgent: false };
}
