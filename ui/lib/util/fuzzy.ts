// Small fuzzy matcher for the host filter: every whitespace-separated token
// must match (as a subsequence) at least one searchable field. Scores favour
// prefix, word-boundary and contiguous matches, and matches in the name.

import type { Host } from "../types";

export interface Match {
  score: number;
  /** Indices of matched characters in host.name, for highlighting. */
  nameIndices: number[];
}

interface FieldMatch {
  score: number;
  indices: number[];
}

function isBoundary(text: string, i: number): boolean {
  if (i === 0) return true;
  const prev = text[i - 1];
  return /[\s\-_.@:/]/.test(prev) || (/[a-z]/.test(prev) && /[A-Z]/.test(text[i]));
}

/** Subsequence match of `query` (lowercase) in `text`. */
export function matchField(query: string, text: string): FieldMatch | null {
  if (!query) return { score: 0, indices: [] };
  const lower = text.toLowerCase();

  // Exact substring gets a strong, predictable score.
  const sub = lower.indexOf(query);
  if (sub !== -1) {
    const indices = Array.from({ length: query.length }, (_, k) => sub + k);
    let score = 100 + query.length * 4;
    if (sub === 0) score += 40;
    else if (isBoundary(text, sub)) score += 20;
    if (query.length === lower.length) score += 30;
    return { score, indices };
  }

  let score = 0;
  let qi = 0;
  let prev = -2;
  const indices: number[] = [];
  for (let i = 0; i < lower.length && qi < query.length; i++) {
    if (lower[i] !== query[qi]) continue;
    let s = 1;
    if (i === prev + 1) s += 5;
    if (isBoundary(text, i)) s += 8;
    score += s;
    indices.push(i);
    prev = i;
    qi++;
  }
  if (qi < query.length) return null;
  // Penalize spread-out matches.
  score -= (indices[indices.length - 1] - indices[0] - query.length + 1) * 0.5;
  return { score, indices };
}

export function matchHost(host: Host, query: string): Match | null {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return { score: 0, nameIndices: [] };

  const fields: [string, number][] = [
    [host.hostname, 0.8],
    [host.user ?? "", 0.6],
    [host.group ?? "", 0.5],
    ...host.tags.map((t): [string, number] => [t, 0.7]),
  ];

  let total = 0;
  const nameIndices = new Set<number>();
  for (const token of tokens) {
    let best = -Infinity;
    const name = matchField(token, host.name);
    if (name) {
      best = name.score;
      name.indices.forEach((i) => nameIndices.add(i));
    }
    for (const [text, weight] of fields) {
      if (!text) continue;
      const m = matchField(token, text);
      if (m && m.score * weight > best) best = m.score * weight;
    }
    if (best === -Infinity) return null;
    total += best;
  }
  return { score: total, nameIndices: [...nameIndices].sort((a, b) => a - b) };
}
