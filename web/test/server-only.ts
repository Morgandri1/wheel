// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

// Stands in for the `server-only` package under Vitest. Its real entry throws everywhere except
// React's server condition, which Vitest does not resolve; Next's build enforces the boundary.
export {};
