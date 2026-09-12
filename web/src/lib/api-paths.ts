// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * How the browser builds an API path out of ids it did not mint. A route param is user input, so an
 * id like `../../auth/me` must never reshape the path: a project id must be a UUID, and every
 * segment is encoded on its own. The proxy and the API refuse traversal as well; this is the first
 * of the three locks.
 */

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isProjectId(id: string): boolean {
  return UUID.test(id);
}

/**
 * `/v1/projects/<id>/<rest…>` with each segment encoded, or null when the project id is not a UUID.
 * An empty, `.` or `..` segment is refused too: encoding cannot protect those, because a browser
 * resolves dot segments — even percent-encoded ones — before the request leaves.
 */
export function projectPath(projectId: string, ...rest: string[]): string | null {
  if (!isProjectId(projectId)) return null;
  if (rest.some((segment) => segment === "" || segment === "." || segment === "..")) return null;
  return ["", "v1", "projects", projectId, ...rest].map(encodeURIComponent).join("/");
}

export function withQuery(path: string | null, params: URLSearchParams): string | null {
  if (path === null) return null;
  const query = params.toString();
  return query ? `${path}?${query}` : path;
}
