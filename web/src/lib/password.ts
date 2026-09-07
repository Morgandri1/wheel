/** Matches the API's own rule (docs/API.md) so the form refuses what the server would refuse. */
export const MIN_PASSWORD_LENGTH = 12;

/**
 * Why the new password is checked against the current one: the API accepts a "change" to the same
 * password and returns 204, having revoked every session. The user would be signed out, told it
 * worked, and have changed nothing — the most confusing possible outcome of a security action.
 */
export function passwordChangeProblem(current: string, next: string, confirm: string): string | null {
  if (!current) return "Enter your current password.";
  if (!next) return "Enter a new password.";
  if (next.length < MIN_PASSWORD_LENGTH) return `Use at least ${MIN_PASSWORD_LENGTH} characters.`;
  if (next === current) return "The new password is the same as the current one.";
  if (confirm !== next) return "The two new passwords do not match.";
  return null;
}
