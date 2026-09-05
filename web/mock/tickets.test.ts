import { describe, expect, it } from "vitest";
import { TICKET_TTL_MS, TicketStore } from "./tickets";

const PROJECT = "p1";
const OTHER = "p2";

describe("ws-ticket", () => {
  it("admits a fresh ticket for the project it was minted for", () => {
    const store = new TicketStore();
    const ticket = store.mint(PROJECT, "t1");
    expect(store.redeem(ticket, PROJECT)).toBe(true);
  });

  it("refuses a replay — a ticket works exactly once", () => {
    const store = new TicketStore();
    const ticket = store.mint(PROJECT, "t1");
    expect(store.redeem(ticket, PROJECT)).toBe(true);
    expect(store.redeem(ticket, PROJECT)).toBe(false);
  });

  it("refuses a ticket minted for a different project", () => {
    const store = new TicketStore();
    const ticket = store.mint(PROJECT, "t1");
    expect(store.redeem(ticket, OTHER)).toBe(false);
  });

  it("refuses a ticket once its 30 seconds are up", () => {
    let clock = 1_000;
    const store = new TicketStore(() => clock);
    const ticket = store.mint(PROJECT, "t1");
    clock += TICKET_TTL_MS + 1;
    expect(store.redeem(ticket, PROJECT)).toBe(false);
  });

  it("refuses a missing or unknown ticket", () => {
    const store = new TicketStore();
    expect(store.redeem(null, PROJECT)).toBe(false);
    expect(store.redeem("", PROJECT)).toBe(false);
    expect(store.redeem("never-minted", PROJECT)).toBe(false);
  });

  it("burns a ticket even when the redemption fails, so a wrong guess is not retryable", () => {
    const store = new TicketStore();
    const ticket = store.mint(PROJECT, "t1");
    expect(store.redeem(ticket, OTHER)).toBe(false);
    expect(store.redeem(ticket, PROJECT)).toBe(false);
    expect(store.size).toBe(0);
  });
});
