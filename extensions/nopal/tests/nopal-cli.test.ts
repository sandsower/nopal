import assert from "node:assert/strict";
import { test } from "node:test";
import {
	fetchLedgerPointer,
	fetchWorkflowShow,
	establishPlot,
	ledgerEvent,
	ledgerFinalize,
	ledgerInit,
	observeAfkRun,
	parseLedgerPointerEnvelope,
	parsePlotEstablishmentEnvelope,
	parseRunObservationEnvelope,
	parseRunSubmitEnvelope,
	parseWorkflowShowEnvelope,
	resolveNopalSessionBinding,
	submitAfkRun,
	type ExecFn,
	type ExecResult,
} from "../nopal-cli.ts";

const plotEstablishmentEnvelope = {
	kind: "nopal.plot_establishment/v1",
	ok: true,
	outcome: "established",
	plot: { plot_id: "plot-1", title: "Plot" },
	diagnostics: [],
};

const submitEnvelope = {
	kind: "nopal.run_submit/v1",
	ok: true,
	submitted: true,
	deduplicated: false,
	manifest_path: ".beislid/exports/bundle/slices/slice.json",
	manifest_sha256: "a".repeat(64),
	decision: "allow",
	placement: "dedicated_run_runtime",
	handle: {
		service_id: "rondo-core",
		repo_id: "nopal.repo/v1:abc",
		plot_id: "plot-1",
		run_id: "run-1",
		status: "running",
		event_cursor: "rondo.core/v1:0",
	},
	diagnostics: [],
};

const observationEnvelope = {
	kind: "nopal.run_observation/v1",
	ok: true,
	handle: { repo_id: "nopal.repo/v1:abc", plot_id: "plot-1", run_id: "run-1" },
	status: "running",
	last_event: { type: "rondo.run.status_changed" },
	evidence_pointers: [{ uri: "rondo-run://run-1/artifacts/execution-request.json" }],
	event_cursor: "rondo.core/v1:2",
	events: [{ type: "rondo.run.status_changed" }],
	next_event_cursor: "rondo.core/v1:1",
	has_more: true,
	settled: false,
	diagnostics: [],
};

function fakeExec(result: Partial<ExecResult> & { stdout: string }): ExecFn {
	return async () => ({ stderr: "", code: 0, ...result });
}

function throwingExec(message: string): ExecFn {
	return async () => {
		throw new Error(message);
	};
}

// ---------------------------------------------------------------------------
// AFK submit / observe
// ---------------------------------------------------------------------------

test("parseRunSubmitEnvelope: accepts the exact success shape and rejects malformed invariants", () => {
	assert.deepEqual(parseRunSubmitEnvelope(submitEnvelope), submitEnvelope);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, kind: "foreign" }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, manifest_sha256: "ABC" }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, submitted: false }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: null }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, diagnostics: [1] }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, decision: "ask" }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, placement: "blocked" }), undefined);
	const alternateService = { ...submitEnvelope, handle: { ...submitEnvelope.handle, service_id: "opaque-service-2" } };
	assert.deepEqual(parseRunSubmitEnvelope(alternateService), alternateService);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, service_id: "service\u0085id" } }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, service_id: "s".repeat(513) } }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, event_cursor: "foreign:0" } }), undefined);
	const maximumCursor = "rondo.core/v1:12345678901234567890";
	const oversizedCursor = "rondo.core/v1:123456789012345678901";
	assert.notEqual(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, event_cursor: maximumCursor } }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, event_cursor: oversizedCursor } }), undefined);
	assert.equal(parseRunSubmitEnvelope({ ...submitEnvelope, handle: { ...submitEnvelope.handle, run_id: "run\u0085id" } }), undefined);
});

test("parseRunSubmitEnvelope: accepts a complete failed report but rejects contradictory failure state", () => {
	const failed = {
		...submitEnvelope,
		ok: false,
		submitted: false,
		deduplicated: false,
		handle: null,
		manifest_path: null,
		manifest_sha256: null,
		decision: null,
		placement: null,
		diagnostics: ["Nopal readiness is not green"],
	};
	assert.deepEqual(parseRunSubmitEnvelope(failed), failed);
	assert.equal(parseRunSubmitEnvelope({ ...failed, deduplicated: true }), undefined);
});

test("parseRunObservationEnvelope: verifies strict shape, settled semantics, and handle echoes", () => {
	const expected = { repo_id: "nopal.repo/v1:abc", plot_id: "plot-1", run_id: "run-1" };
	assert.deepEqual(parseRunObservationEnvelope(observationEnvelope, expected), observationEnvelope);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, kind: "foreign" }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, handle: { ...expected, run_id: "other" } }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, settled: true }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, events: {} }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, next_event_cursor: "" }, expected), undefined);
	assert.notEqual(parseRunObservationEnvelope({ ...observationEnvelope, event_cursor: "rondo.core/v1:01", next_event_cursor: "rondo.core/v1:01" }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, event_cursor: "rondo.core/v1:-1" }, expected), undefined);
	const maximumCursor = "rondo.core/v1:12345678901234567890";
	const oversizedCursor = "rondo.core/v1:123456789012345678901";
	assert.notEqual(parseRunObservationEnvelope({ ...observationEnvelope, event_cursor: maximumCursor, next_event_cursor: maximumCursor }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, event_cursor: oversizedCursor }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, next_event_cursor: oversizedCursor }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, status: "run\u009fstatus" }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, ok: false, has_more: true }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, ok: false, settled: true, has_more: false, status: "completed" }, expected), undefined);
	assert.equal(parseRunObservationEnvelope({ ...observationEnvelope, status: "completed", has_more: false, settled: false }, expected), undefined);
	const failed = {
		...observationEnvelope,
		ok: false,
		status: null,
		last_event: null,
		event_cursor: null,
		events: [],
		next_event_cursor: null,
		has_more: false,
		settled: false,
		diagnostics: ["Rondo Core is unavailable"],
	};
	assert.deepEqual(parseRunObservationEnvelope(failed, expected), failed);
	const terminal = { ...observationEnvelope, status: "completed", has_more: false, settled: true };
	assert.deepEqual(parseRunObservationEnvelope(terminal, expected), terminal);
});

test("submitAfkRun: invokes only the pinned Nopal submit command", async () => {
	let call: { command: string; args: string[]; options?: { cwd?: string; timeout?: number } } | undefined;
	const exec: ExecFn = async (command, args, options) => {
		call = { command, args, options };
		return { stdout: JSON.stringify(submitEnvelope), stderr: "", code: 0 };
	};
	const result = await submitAfkRun(exec, { manifestPath: "exports/slice.json", plotId: "plot-1", cwd: "/repo", timeoutMs: 321 });
	assert.equal(result.ok, true);
	assert.deepEqual(call, {
		command: "nopal",
		args: ["--json", "run", "submit", "--manifest", "exports/slice.json", "--plot-id", "plot-1"],
		options: { cwd: "/repo", timeout: 321 },
	});
});

test("submitAfkRun: default submission leaves outer timeout ownership with Nopal", async () => {
	let options: { cwd?: string; timeout?: number; signal?: AbortSignal } | undefined;
	const exec: ExecFn = async (_command, _args, received) => {
		options = received;
		return { stdout: JSON.stringify(submitEnvelope), stderr: "", code: 0 };
	};
	const result = await submitAfkRun(exec, { manifestPath: "exports/slice.json", plotId: "plot-1", cwd: "/repo" });
	assert.equal(result.ok, true);
	assert.deepEqual(options, { cwd: "/repo" });
});

test("submitAfkRun: explicit direct-call timeout is honored", async () => {
	let timeout: number | undefined;
	const exec: ExecFn = async (_command, _args, options) => {
		timeout = options?.timeout;
		return { stdout: JSON.stringify(submitEnvelope), stderr: "", code: 0 };
	};
	await submitAfkRun(exec, { manifestPath: "exports/slice.json", plotId: "plot-1", cwd: "/repo", timeoutMs: 45_000 });
	assert.equal(timeout, 45_000);
});

test("observeAfkRun: invokes one pinned observation with optional cursor and exact echo validation", async () => {
	const calls: string[][] = [];
	const exec: ExecFn = async (_command, args) => {
		calls.push(args);
		return { stdout: JSON.stringify(observationEnvelope), stderr: "", code: 0 };
	};
	const result = await observeAfkRun(exec, {
		repoId: "nopal.repo/v1:abc",
		plotId: "plot-1",
		runId: "run-1",
		eventCursor: "rondo.core/v1:0",
		cwd: "/repo",
	});
	assert.equal(result.ok, true);
	assert.deepEqual(calls, [["--json", "run", "observe", "--repo-id", "nopal.repo/v1:abc", "--plot-id", "plot-1", "--run-id", "run-1", "--cursor", "rondo.core/v1:0"]]);

	const mismatch = await observeAfkRun(fakeExec({ stdout: JSON.stringify({ ...observationEnvelope, handle: { repo_id: "other", plot_id: "plot-1", run_id: "run-1" } }) }), {
		repoId: "nopal.repo/v1:abc",
		plotId: "plot-1",
		runId: "run-1",
		cwd: "/repo",
	});
	assert.deepEqual(mismatch, { ok: false, error: "Nopal AFK observation returned an invalid envelope" });
});

test("AFK invokers sanitize subprocess failures and reject exit/envelope conflicts", async () => {
	const thrown = await submitAfkRun(throwingExec("TOKEN=secret /private/path"), { manifestPath: "slice.json", plotId: "plot-1", cwd: "/repo" });
	assert.deepEqual(thrown, { ok: false, error: "Nopal AFK submission could not execute the nopal binary" });
	assert.doesNotMatch(JSON.stringify(thrown), /secret|private/u);

	const malformed = await observeAfkRun(fakeExec({ stdout: "not-json", stderr: "PASSWORD=hunter2" }), {
		repoId: "nopal.repo/v1:abc",
		plotId: "plot-1",
		runId: "run-1",
		cwd: "/repo",
	});
	assert.deepEqual(malformed, { ok: false, error: "Nopal AFK observation returned unparseable output" });
	assert.doesNotMatch(JSON.stringify(malformed), /hunter2/u);

	const conflict = await submitAfkRun(fakeExec({ stdout: JSON.stringify(submitEnvelope), code: 1 }), { manifestPath: "slice.json", plotId: "plot-1", cwd: "/repo" });
	assert.deepEqual(conflict, { ok: false, error: "Nopal AFK submission exit status did not match its envelope" });
});

// ---------------------------------------------------------------------------
// workflow show
// ---------------------------------------------------------------------------

test("Plot Establishment parser and invoker preserve the exact Core boundary", async () => {
	assert.deepEqual(parsePlotEstablishmentEnvelope(plotEstablishmentEnvelope), {
		...plotEstablishmentEnvelope,
		plot: { plot_id: "plot-1" },
	});
	assert.equal(parsePlotEstablishmentEnvelope({ ...plotEstablishmentEnvelope, outcome: "moved" }), undefined);
	const calls: Array<{ command: string; args: string[]; cwd?: string }> = [];
	const result = await establishPlot(async (command, args, options) => {
		calls.push({ command, args, cwd: options?.cwd });
		return { stdout: JSON.stringify(plotEstablishmentEnvelope), stderr: "", code: 0 };
	}, { event: "kickoff_context_ready", cwd: "/repo/worktree" });
	assert.equal(result.ok, true);
	assert.deepEqual(calls, [{
		command: "nopal",
		args: ["--json", "plot", "establish", "--event", "kickoff_context_ready", "--workspace", "/repo/worktree"],
		cwd: "/repo/worktree",
	}]);
});

test("Plot Establishment passes a ready structured endpoint through exact CLI flags", async () => {
	const calls: string[][] = [];
	const result = await establishPlot(async (_command, args) => {
		calls.push(args);
		return { stdout: JSON.stringify(plotEstablishmentEnvelope), stderr: "", code: 0 };
	}, {
		event: "kickoff_context_ready",
		cwd: "/repo/worktree",
		protocol: { kind: "nopal.session/v2", transport: "unix", address: "/tmp/nopal-501/session.sock", state: "ready" },
	});

	assert.equal(result.ok, true);
	assert.deepEqual(calls, [[
		"--json", "plot", "establish",
		"--event", "kickoff_context_ready",
		"--workspace", "/repo/worktree",
		"--protocol-kind", "nopal.session/v2",
		"--protocol-address", "/tmp/nopal-501/session.sock",
		"--protocol-state", "ready",
	]]);
});

test("Plot Establishment retains the selected Core Session identity for bridge bootstrap", () => {
	const parsed = parsePlotEstablishmentEnvelope({
		...plotEstablishmentEnvelope,
		plot: { plot_id: "plot-1", selected_session_id: "session-1" },
	});
	assert.deepEqual(parsed?.plot, { plot_id: "plot-1", selected_session_id: "session-1" });
	assert.equal(parsePlotEstablishmentEnvelope({
		...plotEstablishmentEnvelope,
		plot: { plot_id: "plot-1", selected_session_id: " bad " },
	}), undefined);
});

test("resolveNopalSessionBinding reads both Core-stamped identities from the exact tmux pane", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const binding = await resolveNopalSessionBinding(async (command, args) => {
		calls.push({ command, args });
		const option = args.at(-1);
		return { stdout: option === "@nopal_plot" ? "plot-01\n" : "session-01\n", stderr: "", code: 0 };
	}, { cwd: "/repo", paneId: "%7" });

	assert.deepEqual(binding, { plotId: "plot-01", sessionId: "session-01" });
	assert.deepEqual(calls, [
		{ command: "tmux", args: ["show-options", "-qv", "-t", "%7", "@nopal_plot"] },
		{ command: "tmux", args: ["show-options", "-qv", "-t", "%7", "@nopal_plot_session"] },
	]);
});

test("resolveNopalSessionBinding fails closed when either identity is absent or unsafe", async () => {
	assert.equal(await resolveNopalSessionBinding(async () => ({ stdout: "", stderr: "", code: 0 }), { cwd: "/repo", paneId: undefined }), undefined);
	assert.equal(await resolveNopalSessionBinding(async (_command, args) => ({
		stdout: args.at(-1) === "@nopal_plot" ? "plot-01\n" : " bad-session \n",
		stderr: "",
		code: 0,
	}), { cwd: "/repo", paneId: "%7" }), undefined);
});

test("parseWorkflowShowEnvelope: happy path with events/exclude/budget", () => {
	const parsed = parseWorkflowShowEnvelope({
		kind: "nopal.workflow.show/v1",
		ok: true,
		handoff: { auto: true, events: ["kickoff_context_ready"], exclude: ["spec_approved"] },
		babysit: { token_budget: 400000 },
		establishment: { events: ["kickoff_context_ready"] },
		diagnostics: [],
	});
	assert.deepEqual(parsed, {
		handoff: { auto: true, events: ["kickoff_context_ready"], exclude: ["spec_approved"] },
		babysit: { tokenBudget: 400000 },
		establishment: { events: ["kickoff_context_ready"] },
	});
});

test("parseWorkflowShowEnvelope: default envelope (empty events/exclude, null budget)", () => {
	const parsed = parseWorkflowShowEnvelope({
		kind: "nopal.workflow.show/v1",
		ok: true,
		handoff: { auto: false, events: [], exclude: ["break_spec_approved", "spec_approved", "blueprint_approved"] },
		babysit: { token_budget: null },
		diagnostics: [],
	});
	assert.deepEqual(parsed, {
		handoff: { auto: false, events: [], exclude: ["break_spec_approved", "spec_approved", "blueprint_approved"] },
		babysit: { tokenBudget: null },
		establishment: { events: [] },
	});
});

test("parseWorkflowShowEnvelope: ok false is treated as unparseable", () => {
	assert.equal(parseWorkflowShowEnvelope({ ok: false, handoff: { auto: false, events: [], exclude: [] } }), undefined);
});

test("parseWorkflowShowEnvelope: malformed shape returns undefined", () => {
	assert.equal(parseWorkflowShowEnvelope({ ok: true }), undefined);
	assert.equal(parseWorkflowShowEnvelope(null), undefined);
	assert.equal(parseWorkflowShowEnvelope("nope"), undefined);
});

test("parseWorkflowShowEnvelope: non-positive or non-numeric token_budget normalizes to null", () => {
	const parsed = parseWorkflowShowEnvelope({
		ok: true,
		handoff: { auto: false, events: [], exclude: [] },
		babysit: { token_budget: -5 },
	});
	assert.equal(parsed?.babysit.tokenBudget, null);
});

test("fetchWorkflowShow: returns undefined when the binary cannot be executed", async () => {
	const result = await fetchWorkflowShow(throwingExec("spawn nopal ENOENT"), "/repo");
	assert.equal(result, undefined);
});

test("fetchWorkflowShow: returns undefined on nonzero exit", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: false, diagnostics: [{ message: "bad config" }] }), code: 1 });
	const result = await fetchWorkflowShow(exec, "/repo");
	assert.equal(result, undefined);
});

test("fetchWorkflowShow: returns undefined on unparseable stdout", async () => {
	const exec = fakeExec({ stdout: "not json" });
	const result = await fetchWorkflowShow(exec, "/repo");
	assert.equal(result, undefined);
});

test("fetchWorkflowShow: happy path parses through", async () => {
	const exec = fakeExec({
		stdout: JSON.stringify({
			ok: true,
			handoff: { auto: true, events: [], exclude: [] },
			babysit: { token_budget: 250000 },
			establishment: { events: ["kickoff_context_ready"] },
		}),
	});
	const result = await fetchWorkflowShow(exec, "/repo");
	assert.deepEqual(result, {
		handoff: { auto: true, events: [], exclude: [] },
		babysit: { tokenBudget: 250000 },
		establishment: { events: ["kickoff_context_ready"] },
	});
});

// ---------------------------------------------------------------------------
// ledger pointer
// ---------------------------------------------------------------------------

test("parseLedgerPointerEnvelope: happy path with entries", () => {
	const parsed = parseLedgerPointerEnvelope({
		ok: true,
		source: ".nopal/checkpoints/latest.json",
		entries: [
			{
				event: "kickoff_context_ready",
				path: "plans/x.md",
				ticket: { id: "TASK-1", title: "Do the thing" },
				branch: "nopal/task-1",
				source_skill: "kickoff",
				written_at: "2026-07-06T14:00:00Z",
			},
		],
	});
	assert.deepEqual(parsed, {
		source: ".nopal/checkpoints/latest.json",
		entries: [
			{
				event: "kickoff_context_ready",
				path: "plans/x.md",
				ticket: { id: "TASK-1", title: "Do the thing" },
				branch: "nopal/task-1",
				source_skill: "kickoff",
				written_at: "2026-07-06T14:00:00Z",
			},
		],
	});
});

test("parseLedgerPointerEnvelope: empty entries and null source is ok", () => {
	assert.deepEqual(parseLedgerPointerEnvelope({ ok: true, source: null, entries: [] }), { source: null, entries: [] });
});

test("parseLedgerPointerEnvelope: drops entries missing event or path", () => {
	const parsed = parseLedgerPointerEnvelope({
		ok: true,
		source: ".nopal/checkpoints/latest.json",
		entries: [{ event: "e1" }, { path: "p1" }, { event: "e2", path: "p2" }],
	});
	assert.equal(parsed?.entries.length, 1);
	assert.equal(parsed?.entries[0]?.event, "e2");
});

test("parseLedgerPointerEnvelope: ok false or missing entries returns undefined", () => {
	assert.equal(parseLedgerPointerEnvelope({ ok: false, entries: [] }), undefined);
	assert.equal(parseLedgerPointerEnvelope({ ok: true }), undefined);
});

test("fetchLedgerPointer: returns undefined when the CLI could not be consulted", async () => {
	const result = await fetchLedgerPointer(throwingExec("ENOENT"), "/repo");
	assert.equal(result, undefined);
});

test("fetchLedgerPointer: happy path passes cwd through as exec options", async () => {
	let capturedCwd: string | undefined;
	const exec: ExecFn = async (_cmd, _args, options) => {
		capturedCwd = options?.cwd;
		return { stdout: JSON.stringify({ ok: true, source: null, entries: [] }), stderr: "", code: 0 };
	};
	const result = await fetchLedgerPointer(exec, "/repo/root");
	assert.deepEqual(result, { source: null, entries: [] });
	assert.equal(capturedCwd, "/repo/root");
});

// ---------------------------------------------------------------------------
// ledger init / event / finalize
// ---------------------------------------------------------------------------

test("ledgerInit: happy path returns the run id", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: true, run_id: "20260706T190644Z-8a0827" }) });
	const result = await ledgerInit(exec, { skill: "babysit", flow: "babysit", cwd: "/repo" });
	assert.deepEqual(result, { ok: true, runId: "20260706T190644Z-8a0827" });
});

test("ledgerInit: passes skill/flow/ticket args through", async () => {
	let capturedArgs: string[] = [];
	const exec: ExecFn = async (_cmd, args) => {
		capturedArgs = args;
		return { stdout: JSON.stringify({ ok: true, run_id: "run-1" }), stderr: "", code: 0 };
	};
	await ledgerInit(exec, { skill: "babysit", flow: "babysit", ticketId: "TASK-1", ticketTitle: "Do it", cwd: "/repo" });
	assert.deepEqual(capturedArgs, [
		"--json",
		"ledger",
		"init",
		"--skill",
		"babysit",
		"--flow",
		"babysit",
		"--ticket-id",
		"TASK-1",
		"--ticket-title",
		"Do it",
	]);
});

test("ledgerInit: missing run_id in an ok response is a failure", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: true }) });
	const result = await ledgerInit(exec, { skill: "babysit", cwd: "/repo" });
	assert.equal(result.ok, false);
});

test("ledgerInit: nonzero exit with diagnostics surfaces the diagnostic message", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: false, diagnostics: [{ message: "run already exists" }] }), code: 1 });
	const result = await ledgerInit(exec, { skill: "babysit", cwd: "/repo" });
	assert.equal(result.ok, false);
	assert.match((result as { error: string }).error, /run already exists/);
});

test("ledgerInit: binary missing fails without throwing", async () => {
	const result = await ledgerInit(throwingExec("spawn nopal ENOENT"), { skill: "babysit", cwd: "/repo" });
	assert.equal(result.ok, false);
});

test("ledgerEvent: happy path", async () => {
	const exec = fakeExec({ stdout: JSON.stringify({ ok: true, run_id: "run-1", event_type: "babysit_turn" }) });
	const result = await ledgerEvent(exec, { runId: "run-1", type: "babysit_turn", summary: "1234 tokens used", cwd: "/repo" });
	assert.deepEqual(result, { ok: true });
});

test("ledgerEvent: run not found surfaces a failure", async () => {
	const exec = fakeExec({
		stdout: JSON.stringify({ ok: false, diagnostics: [{ severity: "error", code: "run_not_found", message: "run not found: nope" }] }),
		code: 1,
	});
	const result = await ledgerEvent(exec, { runId: "nope", type: "babysit_turn", cwd: "/repo" });
	assert.equal(result.ok, false);
	assert.match((result as { error: string }).error, /run not found/);
});

test("ledgerFinalize: happy path for each status", async () => {
	for (const status of ["completed", "interrupted", "failed"] as const) {
		const exec = fakeExec({ stdout: JSON.stringify({ ok: true, run_id: "run-1", status }) });
		const result = await ledgerFinalize(exec, { runId: "run-1", status, cwd: "/repo" });
		assert.deepEqual(result, { ok: true });
	}
});

test("ledgerFinalize: exec throwing fails without throwing", async () => {
	const result = await ledgerFinalize(throwingExec("ENOENT"), { runId: "run-1", status: "completed", cwd: "/repo" });
	assert.equal(result.ok, false);
});
