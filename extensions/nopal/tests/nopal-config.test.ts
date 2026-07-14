import assert from "node:assert/strict";
import { test } from "node:test";
import type { ExecFn, ExecResult } from "../nopal-cli.ts";
import { loadNopalModule } from "./setup.ts";

const { createNopalConfigCache, FALLBACK_CONFIG, parseTokenBudgetArg, resolveNopalWorkflowConfig, splitBabysitTokenBudgetArg, toWorkflowConfig } =
	await loadNopalModule<typeof import("../nopal-config.ts")>("../nopal-config.ts");

function fakeExec(result: Partial<ExecResult> & { stdout: string }): ExecFn {
	return async () => ({ stderr: "", code: 0, ...result });
}

// ---------------------------------------------------------------------------
// config defaulting fallbacks
// ---------------------------------------------------------------------------

test("toWorkflowConfig: empty events array means 'all' (all events eligible)", () => {
	const config = toWorkflowConfig({
		handoff: { auto: true, events: [], exclude: ["spec_approved"] },
		babysit: { tokenBudget: null },
		establishment: { events: ["kickoff_context_ready"] },
	});
	assert.equal(config.handoff.events, "all");
	assert.deepEqual(config.handoff.exclude, new Set(["spec_approved"]));
	assert.equal(config.handoff.autoHandoff, true);
	assert.deepEqual(config.establishmentEvents, new Set(["kickoff_context_ready"]));
});

test("toWorkflowConfig: non-empty events array becomes a Set", () => {
	const config = toWorkflowConfig({
		handoff: { auto: true, events: ["kickoff_context_ready", "spec_ready"], exclude: [] },
		babysit: { tokenBudget: 1000 },
		establishment: { events: [] },
	});
	assert.deepEqual(config.handoff.events, new Set(["kickoff_context_ready", "spec_ready"]));
	assert.deepEqual(config.handoff.exclude, new Set());
	assert.equal(config.babysitTokenBudget, 1000);
});

test("toWorkflowConfig: undefined result (CLI could not be consulted) falls back to safe defaults", () => {
	const config = toWorkflowConfig(undefined);
	assert.deepEqual(config, FALLBACK_CONFIG);
	assert.equal(config.handoff.autoHandoff, false);
	assert.equal(config.babysitTokenBudget, null);
});

test("resolveNopalWorkflowConfig: binary missing falls back without throwing", async () => {
	const exec: ExecFn = async () => {
		throw new Error("spawn nopal ENOENT");
	};
	const config = await resolveNopalWorkflowConfig(exec, "/repo");
	assert.deepEqual(config, FALLBACK_CONFIG);
});

test("resolveNopalWorkflowConfig: happy path resolves through fetchWorkflowShow", async () => {
	const exec = fakeExec({
		stdout: JSON.stringify({ ok: true, handoff: { auto: true, events: [], exclude: [] }, babysit: { token_budget: 400000 } }),
	});
	const config = await resolveNopalWorkflowConfig(exec, "/repo");
	assert.equal(config.handoff.autoHandoff, true);
	assert.equal(config.handoff.events, "all");
	assert.equal(config.babysitTokenBudget, 400000);
});

test("createNopalConfigCache: memoizes until refresh() is called", async () => {
	let calls = 0;
	const exec: ExecFn = async () => {
		calls += 1;
		return { stdout: JSON.stringify({ ok: true, handoff: { auto: false, events: [], exclude: [] }, babysit: { token_budget: null } }), stderr: "", code: 0 };
	};
	const cache = createNopalConfigCache(exec);
	await cache.get("/repo");
	await cache.get("/repo");
	assert.equal(calls, 1, "second get() before refresh should not re-invoke exec");
	cache.refresh();
	await cache.get("/repo");
	assert.equal(calls, 2, "get() after refresh() should re-invoke exec");
});

// ---------------------------------------------------------------------------
// budget arg splitting
// ---------------------------------------------------------------------------

test("splitBabysitTokenBudgetArg: extracts --tokens=<n> and strips it from args", () => {
	const result = splitBabysitTokenBudgetArg("review this PR --tokens=250k please");
	assert.equal(result.tokenBudget, "250k");
	assert.equal(result.args, "review this PR please");
});

test("splitBabysitTokenBudgetArg: extracts --tokens <n> with a space", () => {
	const result = splitBabysitTokenBudgetArg("--tokens 1.5m watch the release");
	assert.equal(result.tokenBudget, "1.5m");
	assert.equal(result.args, "watch the release");
});

test("splitBabysitTokenBudgetArg: no --tokens flag leaves args untouched", () => {
	const result = splitBabysitTokenBudgetArg("just babysit this PR");
	assert.equal(result.tokenBudget, undefined);
	assert.equal(result.args, "just babysit this PR");
});

test("splitBabysitTokenBudgetArg: invalid budget value is not captured, flag text is still stripped", () => {
	const result = splitBabysitTokenBudgetArg("--tokens notanumber babysit");
	assert.equal(result.tokenBudget, undefined);
});

test("splitBabysitTokenBudgetArg: first valid occurrence wins when repeated", () => {
	const result = splitBabysitTokenBudgetArg("--tokens 100k --tokens 200k babysit");
	assert.equal(result.tokenBudget, "100k");
});

test("parseTokenBudgetArg: parses plain integers, k, and m suffixes", () => {
	assert.equal(parseTokenBudgetArg("400000"), 400000);
	assert.equal(parseTokenBudgetArg("250k"), 250_000);
	assert.equal(parseTokenBudgetArg("1.5m"), 1_500_000);
	assert.equal(parseTokenBudgetArg("2K"), 2_000);
});

test("parseTokenBudgetArg: rejects zero, negative, and malformed values", () => {
	assert.equal(parseTokenBudgetArg("0"), null);
	assert.equal(parseTokenBudgetArg("-5"), null);
	assert.equal(parseTokenBudgetArg("abc"), null);
	assert.equal(parseTokenBudgetArg(undefined), null);
});
