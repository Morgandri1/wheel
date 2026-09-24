// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { Message } from "@/lib/schema";
import { tierAtLeast } from "@/lib/tiers";

/**
 * What a guest is shown where the engine withheld content (finding 062). The engine enforces it —
 * transcript lines never reach a guest and a message body arrives as a placeholder — so this file
 * decides only how to SAY so: as an ordinary state, never an error, and never as "nothing here",
 * which would be false.
 */

export const HIDDEN_LABEL = "Hidden — prompter tier or above";

/**
 * `Message.redacted` is `true` only on a redacted message and absent otherwise, so an unredacted
 * message is byte-identical to before. The body of a redacted one is a fixed placeholder for
 * clients that do not know the flag; this one keys off the flag and never matches that string.
 */
export function isRedactedMessage(message: Message): boolean {
  return message.redacted === true;
}

/** The unfiltered `GET …/log` names what it left out, and only for a caller it left something out for. */
export function redactedStreamsOf(page: { redacted_streams?: string[] }): string[] {
  return page.redacted_streams ?? [];
}

/**
 * Is this agent's transcript hidden from the caller?
 *
 * The log page's marker is the authority, but it is seen only on the initial fetch (a guest's live
 * socket simply never carries transcript frames), so the caller's own tier answers for the rest of
 * the session. An UNKNOWN tier claims nothing: an API that sent no tier must not make an admin's
 * transcript look hidden, and a hidden one that slips through is still empty, not wrong.
 */
export function transcriptHidden(tier: string | undefined, redactedStreams: readonly string[]): boolean {
  if (redactedStreams.includes("transcript")) return true;
  return tier !== undefined && !tierAtLeast(tier, "prompter");
}
