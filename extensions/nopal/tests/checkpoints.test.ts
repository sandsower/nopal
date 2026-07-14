import assert from "node:assert/strict";
import { test } from "node:test";
import type { ExecFn, ExecResult } from "../nopal-cli.ts";
import type { CheckpointPointerSnapshot } from "../checkpoints.ts";
import { loadNopalModule } from "./setup.ts";

const { identityForEntry, pickNewBoundary, readLatestCheckpoint } = await loadNopalModule<typeof import("../checkpoints.ts")>("../checkpoints.ts");

function fakeExec(result: Partial<ExecResult> & { stdout: string }): ExecFn {
	return async () => ({ stderr: "", code: 0, ...result });
}

function snapshot(entries: CheckpointPointerSnapshot["entries"]): CheckpointPointerSnapshot {
	return { source: ".nopal/checkpoints/latest.json", entries, identities: entries.map(identityForEntry) };
}

// ---------------------------------------------------------------------------
// identityForEntry
// ---------------------------------------------------------------------------

test("identityForEntry: identity is stable for identical entries and differs when any field changes", () => {
	const entry = { event: "kickoff_context_ready", path: "plans/x.md", branch: "nopal/task-1", ticket: { id: "TASK-1" }, written_at: "t1" };
	const a = identityForEntry(entry);
	const b = identityForEntry({ ...entry });
	assert.equal(a.id, b.id);

	const differentPath = identityForEntry({ ...entry, path: "plans/y.md" });
	assert.notEqual(a.id, differentPath.id);

	const differentTicket = identityForEntry({ ...entry, ticket: { id: "TASK-2" } });
	assert.notEqual(a.id, differentTicket.id);
});

test("identityForEntry: missing optional fields do not throw and produce a distinct identity from a populated entry", () => {
	const bare = identityForEntry({ event: "envelope_exported", path: "plans/y.md" });
	assert.equal(bare.branch, undefined);
	assert.equal(bare.ticketId, undefined);
	const populated = identityForEntry({ event: "envelope_exported", path: "plans/y.md", branch: "main" });
	assert.notEqual(bare.id, populated.id);
});

// ---------------------------------------------------------------------------
// pickNewBoundary (boundary diffing)
// ---------------------------------------------------------------------------

test("pickNewBoundary: undefined after snapshot yields no boundary", () => {
	const before = snapshot([]);
	assert.equal(pickNewBoundary(before, undefined, "all", new Set(), new Set()), undefined);
});

test("pickNewBoundary: a brand-new entry not present before is picked when events='all'", () => {
	const before = snapshot([]);
	const after = snapshot([{ event: "kickoff_context_ready", path: "plans/x.md" }]);
	const boundary = pickNewBoundary(before, after, "all", new Set(), new Set());
	assert.equal(boundary?.event, "kickoff_context_ready");
});

test("pickNewBoundary: entries already present before are not re-picked", () => {
	const entry = { event: "kickoff_context_ready", path: "plans/x.md" };
	const before = snapshot([entry]);
	const after = snapshot([entry]);
	assert.equal(pickNewBoundary(before, after, "all", new Set(), new Set()), undefined);
});

test("pickNewBoundary: excluded events are never picked even with events='all'", () => {
	const before = snapshot([]);
	const after = snapshot([{ event: "spec_approved", path: "plans/x.md" }]);
	const boundary = pickNewBoundary(before, after, "all", new Set(["spec_approved"]), new Set());
	assert.equal(boundary, undefined);
});

test("pickNewBoundary: an explicit empty exclude set means nothing is excluded (core is the single defaulting authority)", () => {
	const before = snapshot([]);
	const after = snapshot([{ event: "spec_approved", path: "plans/x.md" }]);
	const boundary = pickNewBoundary(before, after, "all", new Set(), new Set());
	assert.equal(boundary?.event, "spec_approved");
});

test("pickNewBoundary: an explicit allowlist restricts eligible events", () => {
	const before = snapshot([]);
	const after = snapshot([
		{ event: "kickoff_context_ready", path: "plans/a.md" },
		{ event: "spec_ready", path: "plans/b.md" },
	]);
	const boundary = pickNewBoundary(before, after, new Set(["spec_ready"]), new Set(), new Set());
	assert.equal(boundary?.event, "spec_ready");
});

test("pickNewBoundary: consumed identities are skipped", () => {
	const before = snapshot([]);
	const after = snapshot([{ event: "kickoff_context_ready", path: "plans/x.md" }]);
	const identity = identityForEntry({ event: "kickoff_context_ready", path: "plans/x.md" });
	const boundary = pickNewBoundary(before, after, "all", new Set(), new Set([identity.id]));
	assert.equal(boundary, undefined);
});

test("pickNewBoundary: when multiple new candidates qualify, the last one wins", () => {
	const before = snapshot([]);
	const after = snapshot([
		{ event: "kickoff_context_ready", path: "plans/a.md" },
		{ event: "spec_ready", path: "plans/b.md" },
	]);
	const boundary = pickNewBoundary(before, after, "all", new Set(), new Set());
	assert.equal(boundary?.event, "spec_ready");
});

// ---------------------------------------------------------------------------
// readLatestCheckpoint (via injected exec)
// ---------------------------------------------------------------------------

test("readLatestCheckpoint: builds identities from the ledger pointer envelope", async () => {
	const exec = fakeExec({
		stdout: JSON.stringify({
			ok: true,
			source: ".nopal/checkpoints/latest.json",
			entries: [{ event: "kickoff_context_ready", path: "plans/x.md", ticket: { id: "TASK-1", title: "T" } }],
		}),
	});
	const result = await readLatestCheckpoint(exec, "/repo");
	assert.equal(result?.source, ".nopal/checkpoints/latest.json");
	assert.equal(result?.entries.length, 1);
	assert.equal(result?.identities[0]?.ticketId, "TASK-1");
});

test("readLatestCheckpoint: neither pointer file existing is an empty (not undefined) snapshot", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: true, source: null, entries: [] }) });
	const result = await readLatestCheckpoint(exec, "/repo");
	assert.deepEqual(result, { source: null, entries: [], identities: [] });
});

test("readLatestCheckpoint: CLI failure returns undefined so callers skip boundary detection", async () => {
	const exec: ExecFn = async () => {
		throw new Error("spawn nopal ENOENT");
	};
	const result = await readLatestCheckpoint(exec, "/repo");
	assert.equal(result, undefined);
});
