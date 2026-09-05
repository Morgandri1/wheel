/**
 * §5 ws-ticket semantics, kept apart from the server so they can be asserted directly.
 *
 * A ticket is the one credential Wheel ever puts in a URL, so it is deliberately weak: it works
 * once, only for the project it was minted for, and only for 30 seconds. The real API must honour
 * the same three rules — ticket-shaped state that outlives a single handshake is just a token.
 */
export const TICKET_TTL_MS = 30_000;

interface Issued {
  projectId: string;
  expiresAt: number;
}

export class TicketStore {
  private readonly issued = new Map<string, Issued>();

  constructor(private readonly now: () => number = Date.now) {}

  mint(projectId: string, id: string): string {
    this.issued.set(id, { projectId, expiresAt: this.now() + TICKET_TTL_MS });
    return id;
  }

  /** Consumes the ticket whether or not it turns out to be valid — one shot, always. */
  redeem(ticket: string | null | undefined, projectId: string): boolean {
    if (!ticket) return false;
    const found = this.issued.get(ticket);
    this.issued.delete(ticket);
    if (!found) return false;
    return found.projectId === projectId && found.expiresAt > this.now();
  }

  get size(): number {
    return this.issued.size;
  }
}
