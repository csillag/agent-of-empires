// Which session views stay mounted while the user is looking at another one.
//
// Order is most-recently-visited first, which is what eviction reads. The DOM
// order is separate (see `renderOrder`): re-parenting a scroll container drops
// its offset, so a promotion must not move a node.

/** Views kept mounted at once. Past this the least recently visited view
 *  unmounts and its next visit is a normal cold load. */
export const KEEP_ALIVE_CAP = 8;

/** The set after visiting `id`: inserted or promoted to the front, capped. */
export function insertMru(kept: readonly string[], id: string, cap: number = KEEP_ALIVE_CAP): string[] {
  return [id, ...kept.filter((k) => k !== id)].slice(0, cap);
}

/** Drop ids that no longer deserve a mounted view (deleted, archived, trashed,
 *  or switched to the terminal view). `keep` is the id on screen and survives
 *  regardless: it leaves on the next switch. */
export function pruneKept(kept: readonly string[], eligible: ReadonlySet<string>, keep: string | null): string[] {
  return kept.filter((id) => id === keep || eligible.has(id));
}

/** DOM order for the kept views. Sorted, so visiting a session reorders the
 *  set without reordering the rendered layers. */
export function renderOrder(kept: readonly string[]): string[] {
  return [...kept].sort();
}

export function sameIds(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}
