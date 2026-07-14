/**
 * Checkpoint boundary detection for the Nopal session extension.
 *
 * Ported from beislid's `checkpoints.ts`: `BoundaryIdentity`, consumed-set
 * dedup, and `pickNewBoundary` diffing are unchanged in spirit. Snapshots
 * now come from `nopal --json ledger pointer` instead of parsing
 * `.beislid/checkpoints/latest.json` directly - JSON parsing and path
 * safety (dropping absolute/`..`/empty paths) are core-owned now, so this
 * module only builds boundary identities and diffs them.
 */

import { fetchLedgerPointer, type ExecFn, type PointerEntry } from "./nopal-cli.js";

export type LatestCheckpointEntry = PointerEntry;

export type BoundaryIdentity = {
	event: string;
	path: string;
	branch?: string;
	ticketId?: string;
	writtenAt?: string;
	id: string;
};

export type CheckpointPointerSnapshot = {
	/** Relative path of the pointer file that was read, or null when neither location exists. */
	source: string | null;
	entries: LatestCheckpointEntry[];
	identities: BoundaryIdentity[];
};

export function identityForEntry(entry: LatestCheckpointEntry): BoundaryIdentity {
	const ticketId = entry.ticket?.id;
	const parts = [entry.event, entry.path, entry.branch ?? "", ticketId ?? "", entry.written_at ?? ""];
	return {
		event: entry.event,
		path: entry.path,
		branch: entry.branch,
		ticketId,
		writtenAt: entry.written_at,
		id: parts.join("|"),
	};
}

/**
 * Snapshot the checkpoint pointer via `nopal --json ledger pointer`.
 * Returns undefined when the CLI could not be consulted (missing binary,
 * nonzero exit, unparseable output) so callers can skip boundary detection
 * for this run rather than acting on stale or partial data.
 */
export async function readLatestCheckpoint(exec: ExecFn, cwd: string): Promise<CheckpointPointerSnapshot | undefined> {
	const result = await fetchLedgerPointer(exec, cwd);
	if (!result) return undefined;
	return { source: result.source, entries: result.entries, identities: result.entries.map(identityForEntry) };
}

/**
 * Pick the newest checkpoint boundary that appeared between `before` and
 * `after`, is allowed by `allowedEvents`, is not in `excludedEvents`, and
 * has not already been consumed. Defaulting (which events are excluded by
 * default) is the core's job via `nopal workflow show`; this function
 * trusts `excludedEvents` as given.
 */
export function pickNewBoundary(
	before: CheckpointPointerSnapshot | undefined,
	after: CheckpointPointerSnapshot | undefined,
	allowedEvents: Set<string> | "all",
	excludedEvents: Set<string>,
	consumed: Set<string>,
): BoundaryIdentity | undefined {
	if (!after) return undefined;
	const beforeIds = new Set(before?.identities.map((identity) => identity.id) ?? []);
	const candidates = after.identities.filter((identity) => {
		if (excludedEvents.has(identity.event)) return false;
		if (allowedEvents !== "all" && !allowedEvents.has(identity.event)) return false;
		if (consumed.has(identity.id)) return false;
		return !beforeIds.has(identity.id);
	});
	return candidates.at(-1);
}
