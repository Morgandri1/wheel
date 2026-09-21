// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { isRedactedMessage, redactedStreamsOf, transcriptHidden } from "./redaction";
import type { Message } from "@/lib/schema";

const message = (extra: object = {}) =>
  ({ id: "m1", body: "hello", bytes: 5, sha256: "abc", ...extra }) as unknown as Message;

describe("isRedactedMessage — the flag, not the placeholder text", () => {
  it("is true only for redacted: true", () => {
    expect(isRedactedMessage(message({ redacted: true }))).toBe(true);
  });

  it("is false when the flag is absent, which is how an unredacted message arrives", () => {
    expect(isRedactedMessage(message())).toBe(false);
  });

  it.each([false, "true", 1, null])("is false for redacted: %j — nothing else counts", (redacted) => {
    expect(isRedactedMessage(message({ redacted }))).toBe(false);
  });

  it("does not treat a message that merely QUOTES the placeholder as hidden", () => {
    expect(isRedactedMessage(message({ body: "[hidden: prompter tier or above]" }))).toBe(false);
  });
});

describe("redactedStreamsOf", () => {
  it("reads the marker the engine sets only for a caller it withheld something from", () => {
    expect(redactedStreamsOf({ redacted_streams: ["transcript"] })).toEqual(["transcript"]);
  });

  it("is empty when the key is absent, as it is for prompter and admin", () => {
    expect(redactedStreamsOf({})).toEqual([]);
  });
});

describe("transcriptHidden", () => {
  it("is hidden when the log page said so, whatever the tier claims", () => {
    expect(transcriptHidden("admin", ["transcript"])).toBe(true);
    expect(transcriptHidden(undefined, ["transcript"])).toBe(true);
  });

  it("is hidden for a guest with no marker: the live socket never carries the marker", () => {
    expect(transcriptHidden("guest", [])).toBe(true);
  });

  it.each(["prompter", "admin"])("is visible for %s", (tier) => {
    expect(transcriptHidden(tier, [])).toBe(false);
  });

  it("claims nothing for an unknown tier: absence is not evidence of a guest", () => {
    expect(transcriptHidden(undefined, [])).toBe(false);
  });

  it("is not moved by a marker for some other stream", () => {
    expect(transcriptHidden("prompter", ["stderr"])).toBe(false);
  });

  it("treats a tier this build does not know as a guest, never rounding it up", () => {
    expect(transcriptHidden("viewer", [])).toBe(true);
  });
});
