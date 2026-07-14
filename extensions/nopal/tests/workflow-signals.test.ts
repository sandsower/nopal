import assert from "node:assert/strict";
import { test } from "node:test";
import type { ExecFn, ExecResult } from "../nopal-cli.ts";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { loadNopalModule } from "./setup.ts";

const { beislidCliShellOutEnabled, createWorkflowSignals, initialSignalForSkill, parseWorkflowSignalsFromCommand, surfaceWorkflowSignal } =
	await loadNopalModule<typeof import("../workflow-signals.ts")>("../workflow-signals.ts");

function fakeExec(result: Partial<ExecResult> & { stdout: string }): ExecFn {
	return async () => ({ stderr: "", code: 0, ...result });
}

function fakeCtx(hasUI = true) {
	const statuses: Record<string, string | undefined> = {};
	let title: string | undefined;
	const ctx = {
		hasUI,
		cwd: "/repo",
		ui: {
			setStatus(key: string, text: string | undefined) {
				statuses[key] = text;
			},
			setTitle(value: string) {
				title = value;
			},
		},
	} as unknown as ExtensionContext;
	return { ctx, statuses, getTitle: () => title };
}

// ---------------------------------------------------------------------------
// initialSignalForSkill
// ---------------------------------------------------------------------------

test("initialSignalForSkill: known skills map to their configured initial state", () => {
	assert.deepEqual(initialSignalForSkill("babysit"), { state: "working", skill: "babysit", phase: "start" });
	assert.deepEqual(initialSignalForSkill("debug"), { state: "explore", skill: "debug", phase: "start" });
});

// ---------------------------------------------------------------------------
// surfaceWorkflowSignal (pure UI)
// ---------------------------------------------------------------------------

test("surfaceWorkflowSignal: sets status and title when UI is available", () => {
	const { ctx, statuses, getTitle } = fakeCtx(true);
	surfaceWorkflowSignal(ctx, { state: "working", skill: "kickoff", phase: "start" });
	assert.match(statuses["nopal-workflow"] ?? "", /working/);
	assert.match(getTitle() ?? "", /kickoff/);
});

test("surfaceWorkflowSignal: no-ops when there is no UI (print/RPC mode)", () => {
	const { ctx, statuses, getTitle } = fakeCtx(false);
	surfaceWorkflowSignal(ctx, { state: "blocked" });
	assert.equal(statuses["nopal-workflow"], undefined);
	assert.equal(getTitle(), undefined);
});

// ---------------------------------------------------------------------------
// beislidCliShellOutEnabled (env flag)
// ---------------------------------------------------------------------------

test("beislidCliShellOutEnabled: defaults on when unset", () => {
	assert.equal(beislidCliShellOutEnabled({}), true);
});

test("beislidCliShellOutEnabled: '0' and 'false' (any case) disable it", () => {
	assert.equal(beislidCliShellOutEnabled({ NOPAL_SIGNALS_BEISLID_CLI: "0" }), false);
	assert.equal(beislidCliShellOutEnabled({ NOPAL_SIGNALS_BEISLID_CLI: "false" }), false);
	assert.equal(beislidCliShellOutEnabled({ NOPAL_SIGNALS_BEISLID_CLI: "FALSE" }), false);
});

test("beislidCliShellOutEnabled: any other value keeps it on", () => {
	assert.equal(beislidCliShellOutEnabled({ NOPAL_SIGNALS_BEISLID_CLI: "1" }), true);
	assert.equal(beislidCliShellOutEnabled({ NOPAL_SIGNALS_BEISLID_CLI: "yes" }), true);
});

// ---------------------------------------------------------------------------
// parseWorkflowSignalsFromCommand (signal parsing from bash mirroring)
// ---------------------------------------------------------------------------

test("parseWorkflowSignalsFromCommand: parses a single emit with skill and phase", () => {
	const signals = parseWorkflowSignalsFromCommand("beislid workflow-signal emit waiting --skill ready-for-review --phase approval");
	assert.deepEqual(signals, [{ state: "waiting", skill: "ready-for-review", phase: "approval", event: undefined }]);
});

test("parseWorkflowSignalsFromCommand: parses multiple emits chained with &&", () => {
	const signals = parseWorkflowSignalsFromCommand(
		"beislid workflow-signal emit working --skill poke-holes --phase interrogate && echo done && beislid workflow-signal emit done --skill poke-holes",
	);
	assert.equal(signals.length, 2);
	assert.equal(signals[0]?.state, "working");
	assert.equal(signals[1]?.state, "done");
});

test("parseWorkflowSignalsFromCommand: unrelated commands produce no signals", () => {
	assert.deepEqual(parseWorkflowSignalsFromCommand("git status"), []);
});

test("parseWorkflowSignalsFromCommand: no flags at all still parses the state", () => {
	assert.deepEqual(parseWorkflowSignalsFromCommand("beislid workflow-signal emit done"), [
		{ state: "done", skill: undefined, phase: undefined, event: undefined },
	]);
});

// ---------------------------------------------------------------------------
// createWorkflowSignals (ledger mirroring)
// ---------------------------------------------------------------------------

test("createWorkflowSignals: mirrors an emitted signal to the ledger when a run is active", async () => {
	const calls: string[][] = [];
	const exec: ExecFn = async (_cmd, args) => {
		calls.push(args);
		return { stdout: JSON.stringify({ ok: true, run_id: "run-1" }), stderr: "", code: 0 };
	};
	const signals = createWorkflowSignals(exec, {});
	signals.setActiveLedgerRun({ runId: "run-1", flow: "babysit", cwd: "/repo" });
	const { ctx } = fakeCtx(false);
	signals.emitWorkflowSignal(ctx, { state: "working", skill: "babysit" });
	// The ledger append is fire-and-forget; flush microtasks before asserting.
	await new Promise((resolve) => setImmediate(resolve));
	assert.equal(calls.length, 1);
	assert.ok(calls[0]?.includes("run-1"));
	assert.ok(calls[0]?.includes("workflow_signal"));
});

test("createWorkflowSignals: does not touch the ledger when no run is active", async () => {
	let called = false;
	const exec: ExecFn = async () => {
		called = true;
		return { stdout: JSON.stringify({ ok: true }), stderr: "", code: 0 };
	};
	const signals = createWorkflowSignals(exec, {});
	const { ctx } = fakeCtx(false);
	signals.emitWorkflowSignal(ctx, { state: "working" });
	await new Promise((resolve) => setImmediate(resolve));
	assert.equal(called, false);
});

test("createWorkflowSignals: surfaceWorkflowSignalsFromCommand mirrors bash-detected signals to the ledger too", async () => {
	const calls: string[][] = [];
	const exec: ExecFn = async (_cmd, args) => {
		calls.push(args);
		return { stdout: JSON.stringify({ ok: true }), stderr: "", code: 0 };
	};
	const signals = createWorkflowSignals(exec, {});
	signals.setActiveLedgerRun({ runId: "run-2", cwd: "/repo" });
	const { ctx } = fakeCtx(false);
	signals.surfaceWorkflowSignalsFromCommand(ctx, "beislid workflow-signal emit review --skill fresh-eyes");
	await new Promise((resolve) => setImmediate(resolve));
	assert.equal(calls.length, 1);
	assert.ok(calls[0]?.includes("run-2"));
});
