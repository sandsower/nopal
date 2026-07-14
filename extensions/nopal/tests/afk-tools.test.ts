import assert from "node:assert/strict";
import { lstat } from "node:fs/promises";
import { createConnection } from "node:net";
import { test } from "node:test";
import type { ExecFn, ExecResult } from "../nopal-cli.ts";
import { loadNopalModule } from "./setup.ts";

const afkTools = await loadNopalModule<typeof import("../afk-tools.ts")>("../afk-tools.js");

const REPO_ID = "nopal.repo/v1:abc";
const PLOT_ID = "plot-1";
const RUN_ID = "run-1";

function observation(options: {
	status?: string;
	events?: unknown[];
	next?: string;
	hasMore?: boolean;
	settled?: boolean;
} = {}) {
	return {
		kind: "nopal.run_observation/v1",
		ok: true,
		handle: { repo_id: REPO_ID, plot_id: PLOT_ID, run_id: RUN_ID },
		status: options.status ?? "running",
		last_event: options.events?.at(-1) ?? null,
		evidence_pointers: [],
		event_cursor: options.next ?? "rondo.core/v1:0",
		events: options.events ?? [],
		next_event_cursor: options.next ?? "rondo.core/v1:0",
		has_more: options.hasMore ?? false,
		settled: options.settled ?? false,
		diagnostics: [],
	};
}

function queuedExec(values: Array<unknown | Partial<ExecResult>>, calls: Array<{ command: string; args: string[] }> = []): ExecFn {
	return async (command, args) => {
		calls.push({ command, args });
		const value = values.shift();
		if (value === undefined) throw new Error("unexpected extra exec");
		if (typeof value === "object" && value !== null && "stdout" in value) {
			return { stderr: "", code: 0, ...(value as Partial<ExecResult>) } as ExecResult;
		}
		return { stdout: JSON.stringify(value), stderr: "", code: 0 };
	};
}

function params(overrides: Partial<import("../afk-tools.ts").AfkResultParams> = {}) {
	return { repoId: REPO_ID, plotId: PLOT_ID, runId: RUN_ID, cwd: "/repo", ...overrides };
}

test("readAfkResult: nonblocking performs exactly one observation", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const exec = queuedExec([observation({ events: [{ n: 1 }], next: "rondo.core/v1:1", hasMore: true })], calls);
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: false }));
	assert.equal(result.outcome, "observed");
	assert.equal(result.polls, 1);
	assert.equal(result.has_more, true);
	assert.deepEqual(result.events, [{ n: 1 }]);
	assert.deepEqual(calls[0], {
		command: "nopal",
		args: ["--json", "run", "observe", "--repo-id", REPO_ID, "--plot-id", PLOT_ID, "--run-id", RUN_ID, "--cursor", "rondo.core/v1:0"],
	});
});

test("readAfkResult: nonblocking applies caller timeout to its one-shot exec", async () => {
	let capturedTimeout: number | undefined;
	const exec: ExecFn = async (_command, _args, options) => {
		capturedTimeout = options?.timeout;
		return { stdout: JSON.stringify(observation()), stderr: "", code: 0 };
	};
	const result = await afkTools.readAfkResult(exec, params({ block: false, timeoutMs: 321 }));
	assert.equal(result.outcome, "observed");
	assert.equal(capturedTimeout, 321);
});

test("readAfkResult: nonblocking passes the full 60s caller budget", async () => {
	let capturedTimeout: number | undefined;
	const exec: ExecFn = async (_command, _args, options) => {
		capturedTimeout = options?.timeout;
		return { stdout: JSON.stringify(observation()), stderr: "", code: 0 };
	};
	await afkTools.readAfkResult(exec, params({ block: false, timeoutMs: 60_000 }));
	assert.equal(capturedTimeout, 60_000);
});

test("readAfkResult: blocking passes each one-shot the remaining total budget", async () => {
	let now = 0;
	const timeouts: number[] = [];
	const pages = [
		observation({ next: "rondo.core/v1:0" }),
		observation({ status: "completed", next: "rondo.core/v1:0", settled: true }),
	];
	const exec: ExecFn = async (_command, _args, options) => {
		timeouts.push(options?.timeout ?? -1);
		return { stdout: JSON.stringify(pages.shift()), stderr: "", code: 0 };
	};
	const result = await afkTools.readAfkResult(exec, params({ block: true, timeoutMs: 1_000, pollIntervalMs: 100 }), undefined, {
		now: () => now,
		sleep: async (milliseconds) => {
			now += milliseconds;
			return true;
		},
	});
	assert.equal(result.outcome, "settled");
	assert.deepEqual(timeouts, [1_000, 900]);
});

test("readAfkResult: blocking drains pages immediately and sleeps only when caught up", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const exec = queuedExec(
		[
			observation({ events: [{ n: 1 }], next: "rondo.core/v1:1", hasMore: true }),
			observation({ events: [{ n: 2 }], next: "rondo.core/v1:2", hasMore: false }),
			observation({ status: "completed", events: [{ n: 3 }], next: "rondo.core/v1:3", settled: true }),
		],
		calls,
	);
	let now = 0;
	const sleeps: number[] = [];
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: true, timeoutMs: 1_000, pollIntervalMs: 25 }), undefined, {
		now: () => now,
		sleep: async (milliseconds) => {
			sleeps.push(milliseconds);
			now += milliseconds;
			return true;
		},
	});
	assert.equal(result.outcome, "settled");
	assert.equal(result.polls, 3);
	assert.deepEqual(result.events, [{ n: 1 }, { n: 2 }, { n: 3 }]);
	assert.equal(result.next_event_cursor, "rondo.core/v1:3");
	assert.deepEqual(sleeps, [25]);
	assert.deepEqual(
		calls.map((call) => call.args.at(-1)),
		["rondo.core/v1:0", "rondo.core/v1:1", "rondo.core/v1:2"],
	);
});

test("readAfkResult: blocking terminal page returns without sleeping", async () => {
	const exec = queuedExec([observation({ status: "failed", next: "rondo.core/v1:4", settled: true })]);
	let slept = false;
	const result = await afkTools.readAfkResult(exec, params({ block: true, eventCursor: "rondo.core/v1:4" }), undefined, {
		sleep: async () => {
			slept = true;
			return true;
		},
	});
	assert.equal(result.outcome, "settled");
	assert.equal(result.status, "failed");
	assert.equal(slept, false);
});

test("readAfkResult: timeout returns the advancing resumable cursor", async () => {
	const exec = queuedExec([observation({ events: [{ n: 1 }], next: "rondo.core/v1:1" })]);
	let now = 0;
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: true, timeoutMs: 10, pollIntervalMs: 10 }), undefined, {
		now: () => now,
		sleep: async (milliseconds) => {
			now += milliseconds;
			return true;
		},
	});
	assert.equal(result.outcome, "timeout");
	assert.equal(result.ok, true);
	assert.equal(result.next_event_cursor, "rondo.core/v1:1");
	assert.equal(result.polls, 1);
});

test("readAfkResult: a one-shot subprocess reaching the total deadline reports timeout", async () => {
	let now = 0;
	const exec: ExecFn = async () => {
		now = 10;
		return { stdout: "", stderr: "local timeout", code: 1, killed: true };
	};
	const result = await afkTools.readAfkResult(exec, params({ block: true, timeoutMs: 10 }), undefined, { now: () => now });
	assert.equal(result.outcome, "timeout");
	assert.equal(result.ok, true);
	assert.equal(result.polls, 1);
});

test("readAfkResult: abort stops local observation and never invokes cancellation", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const exec = queuedExec([observation({ next: "rondo.core/v1:0" })], calls);
	const controller = new AbortController();
	const result = await afkTools.readAfkResult(exec, params({ block: true }), controller.signal, {
		sleep: async () => {
			controller.abort();
			return false;
		},
	});
	assert.equal(result.outcome, "aborted");
	assert.equal(result.ok, true);
	assert.match(result.diagnostics.at(-1) ?? "", /was not cancelled/u);
	assert.equal(calls.length, 1);
	assert.ok(calls.every((call) => call.args.includes("observe") && !call.args.some((arg) => /cancel|terminate|kill/u.test(arg))));
});

test("readAfkResult: AbortSignal cancels an in-flight one-shot observation", async () => {
	const controller = new AbortController();
	let receivedSignal: AbortSignal | undefined;
	const exec: ExecFn = async (_command, _args, options) => {
		receivedSignal = options?.signal;
		return new Promise((resolve) => {
			options?.signal?.addEventListener(
				"abort",
				() => resolve({ stdout: "", stderr: "cancelled locally", code: 1, killed: true }),
				{ once: true },
			);
		});
	};
	const pending = afkTools.readAfkResult(exec, params({ block: true }), controller.signal);
	setTimeout(() => controller.abort(), 0);
	const result = await pending;
	assert.equal(receivedSignal, controller.signal);
	assert.equal(result.outcome, "aborted");
	assert.equal(result.polls, 1);
	assert.match(result.diagnostics.at(-1) ?? "", /was not cancelled/u);
});

test("readAfkResult: rejects every nonadvancing page that claims events remain or contains events", async () => {
	for (const page of [
		observation({ events: [], next: "rondo.core/v1:0", hasMore: true }),
		observation({ events: [{ duplicate: true }], next: "rondo.core/v1:0", hasMore: false }),
	]) {
		const result = await afkTools.readAfkResult(queuedExec([page]), params({ eventCursor: "rondo.core/v1:0", block: true }));
		assert.equal(result.outcome, "cursor_stalled");
		assert.equal(result.ok, false);
		assert.deepEqual(result.events, []);
		assert.equal(result.next_event_cursor, "rondo.core/v1:0");
	}
});

test("readAfkResult: rejects forward jumps, backward cursors, and zero-event advancement", async () => {
	const cases = [
		{
			cursor: "rondo.core/v1:5",
			page: observation({ events: [{ n: 1 }], next: "rondo.core/v1:7" }),
		},
		{
			cursor: "rondo.core/v1:5",
			page: observation({ events: [{ n: 1 }], next: "rondo.core/v1:4" }),
		},
		{
			cursor: "rondo.core/v1:5",
			page: observation({ events: [], next: "rondo.core/v1:6" }),
		},
	];
	for (const value of cases) {
		const result = await afkTools.readAfkResult(queuedExec([value.page]), params({ eventCursor: value.cursor, block: true }));
		assert.equal(result.outcome, "cursor_stalled");
		assert.equal(result.ok, false);
		assert.deepEqual(result.events, []);
		assert.equal(result.next_event_cursor, value.cursor);
	}
});

test("readAfkResult: BigInt cursor accounting accepts the 20-digit contract maximum", async () => {
	const offset = 18_446_744_073_709_551_615n;
	const cursor = `rondo.core/v1:${offset}`;
	const next = `rondo.core/v1:${offset + 2n}`;
	const result = await afkTools.readAfkResult(
		queuedExec([observation({ events: [{ n: 1 }, { n: 2 }], next })]),
		params({ eventCursor: cursor, block: false }),
	);
	assert.equal(result.outcome, "observed");
	assert.equal(result.next_event_cursor, next);
	assert.deepEqual(result.events, [{ n: 1 }, { n: 2 }]);
});

test("readAfkResult: rejects a 21-digit cursor before subprocess contact", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const result = await afkTools.readAfkResult(
		queuedExec([], calls),
		params({ eventCursor: "rondo.core/v1:123456789012345678901", block: false }),
	);
	assert.equal(result.outcome, "error");
	assert.equal(result.next_event_cursor, null);
	assert.deepEqual(calls, []);
});

test("readAfkResult: accepts a leading-zero cursor permitted by Rondo Core", async () => {
	const cursor = "rondo.core/v1:01";
	const result = await afkTools.readAfkResult(
		queuedExec([observation({ next: cursor })]),
		params({ eventCursor: cursor, block: false }),
	);
	assert.equal(result.outcome, "observed");
	assert.equal(result.next_event_cursor, cursor);
});

test("readAfkResult: absent input cursor is accounted from rondo.core/v1:0", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const result = await afkTools.readAfkResult(
		queuedExec([observation({ events: [{ n: 1 }, { n: 2 }], next: "rondo.core/v1:2" })], calls),
		params({ block: false }),
	);
	assert.equal(result.outcome, "observed");
	assert.equal(result.next_event_cursor, "rondo.core/v1:2");
	assert.equal(calls[0]?.args.includes("--cursor"), false);
});

test("readAfkResult: event-count accumulation budget never skips an unreturned page", async () => {
	const exec = queuedExec([
		observation({ events: [{ n: 1 }], next: "rondo.core/v1:1", hasMore: true }),
		observation({ events: [{ n: 2 }], next: "rondo.core/v1:2", hasMore: true }),
	]);
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: true }), undefined, { maxEvents: 1 });
	assert.equal(result.outcome, "budget_exhausted");
	assert.equal(result.ok, true);
	assert.deepEqual(result.events, [{ n: 1 }]);
	assert.equal(result.next_event_cursor, "rondo.core/v1:1");
	assert.equal(result.has_more, true);
});

test("readAfkResult: serialized-byte budget is enforced without partial cursor advance", async () => {
	const event = { payload: "1234567890" };
	const exec = queuedExec([observation({ events: [event], next: "rondo.core/v1:1" })]);
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: true }), undefined, { maxEventBytes: 5 });
	assert.equal(result.outcome, "budget_exhausted");
	assert.deepEqual(result.events, []);
	assert.equal(result.next_event_cursor, "rondo.core/v1:0");
});

test("readAfkResult: serialized-byte accounting includes commas between duplicate events", async () => {
	const event = { duplicate: true };
	const withoutComma = 2 + Buffer.byteLength(JSON.stringify(event), "utf8") * 2;
	const exec = queuedExec([observation({ events: [event, event], next: "rondo.core/v1:2" })]);
	const result = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:0", block: true }), undefined, { maxEventBytes: withoutComma });
	assert.equal(result.outcome, "budget_exhausted");
	assert.deepEqual(result.events, []);
	assert.equal(result.next_event_cursor, "rondo.core/v1:0");
});

test("readAfkResult: malformed envelopes and echo mismatches fail safely", async () => {
	for (const value of [
		{ ...observation(), kind: "foreign" },
		{ ...observation(), handle: { repo_id: REPO_ID, plot_id: PLOT_ID, run_id: "other" } },
		{ ...observation(), next_event_cursor: "not-a-cursor" },
	]) {
		const result = await afkTools.readAfkResult(queuedExec([value]), params());
		assert.equal(result.outcome, "error");
		assert.equal(result.ok, false);
		assert.match(result.diagnostics.at(-1) ?? "", /invalid envelope/u);
	}
});

test("readAfkResult: invalid identifiers and cursors are rejected without echo or subprocess contact", async () => {
	let calls = 0;
	const exec: ExecFn = async () => {
		calls += 1;
		throw new Error("must not execute");
	};
	const invalidId = await afkTools.readAfkResult(exec, params({ runId: "run\u0085forged" }));
	assert.equal(invalidId.outcome, "error");
	assert.equal(invalidId.handle.run_id, "-");
	assert.doesNotMatch(JSON.stringify(invalidId), /forged/u);
	const invalidCursor = await afkTools.readAfkResult(exec, params({ eventCursor: "rondo.core/v1:-1" }));
	assert.equal(invalidCursor.outcome, "error");
	assert.equal(invalidCursor.next_event_cursor, null);
	assert.equal(calls, 0);
});

test("registerAfkTools: registers both UI-independent tools and start uses only submit", async () => {
	const tools: Record<string, any> = {};
	const calls: Array<{ command: string; args: string[] }> = [];
	const submit = {
		kind: "nopal.run_submit/v1",
		ok: true,
		submitted: true,
		deduplicated: false,
		manifest_path: "exports/slice.json",
		manifest_sha256: "a".repeat(64),
		decision: "allow",
		placement: "dedicated_run_runtime",
		handle: { service_id: "opaque-service-2", repo_id: REPO_ID, plot_id: PLOT_ID, run_id: RUN_ID, status: "running", event_cursor: "rondo.core/v1:0" },
		diagnostics: [],
	};
	afkTools.registerAfkTools(
		{
			registerTool(tool: any) {
				tools[tool.name] = tool;
			},
		} as any,
		queuedExec([submit], calls),
		{
			Object: (properties: Record<string, unknown>) => ({ type: "object", properties }),
			String: (options?: Record<string, unknown>) => ({ type: "string", ...options }),
			Optional: (schema: unknown) => schema,
			Boolean: (options?: Record<string, unknown>) => ({ type: "boolean", ...options }),
			Integer: (options?: Record<string, unknown>) => ({ type: "integer", ...options }),
		},
	);
	assert.deepEqual(Object.keys(tools).sort(), ["nopal_afk_result", "nopal_afk_start"]);
	assert.deepEqual(tools.nopal_afk_result.parameters.properties.eventCursor, {
		type: "string",
		minLength: 15,
		maxLength: 34,
		pattern: "^rondo\\.core/v1:[0-9]{1,20}$",
		description: "Opaque cursor from a prior start or result",
	});
	const result = await tools.nopal_afk_start.execute("tc", { manifestPath: "exports/slice.json", plotId: PLOT_ID }, undefined, undefined, { cwd: "/repo", hasUI: false });
	assert.equal(result.isError, false);
	assert.equal(result.details.kind, "nopal.run_submit/v1");
	assert.equal(result.details.handle.service_id, "opaque-service-2");
	assert.deepEqual(calls, [{ command: "nopal", args: ["--json", "run", "submit", "--manifest", "exports/slice.json", "--plot-id", PLOT_ID] }]);
});

test("Nopal extension entrypoint registers both AFK tools without requiring UI state", async () => {
	const extension = await loadNopalModule<{ default: (pi: any) => void }>("../index.js");
	const tools: Record<string, any> = {};
	extension.default({
		registerTool(tool: any) {
			tools[tool.name] = tool;
		},
		registerCommand() {},
		on() {},
		exec: async () => ({ stdout: "", stderr: "", code: 1, killed: false }),
		sendUserMessage: async () => {},
		appendEntry() {},
	});
	assert.ok(tools.nopal_afk_start);
	assert.ok(tools.nopal_afk_result);
});

test("Nopal extension establishes the selected Plot at a configured checkpoint boundary", async () => {
	const extension = await loadNopalModule<{ default: (pi: any) => void }>("../index.js");
	const commands: Record<string, any> = {};
	const handlers: Record<string, Array<(event: unknown, ctx: any) => Promise<void> | void>> = {};
	const calls: string[][] = [];
	let pointerReads = 0;
	const previousSignals = process.env.NOPAL_BEISLID_SIGNALS;
	process.env.NOPAL_BEISLID_SIGNALS = "0";
	try {
		extension.default({
			registerTool() {},
			registerCommand(name: string, command: any) {
				commands[name] = command;
			},
			on(event: string, handler: any) {
				(handlers[event] ??= []).push(handler);
			},
			exec: async (_command: string, args: string[]) => {
				calls.push(args);
				if (args.includes("workflow")) {
					return {
						stdout: JSON.stringify({
							ok: true,
							handoff: { auto: false, events: [], exclude: [] },
							babysit: { token_budget: null },
							establishment: { events: ["kickoff_context_ready"] },
						}),
						stderr: "",
						code: 0,
					};
				}
				if (args.includes("pointer")) {
					pointerReads += 1;
					return {
						stdout: JSON.stringify({
							ok: true,
							source: ".nopal/checkpoints/latest.json",
							entries: pointerReads === 1 ? [] : [{
								event: "kickoff_context_ready",
								path: "plans/kickoff-context-TASK-51.md",
								written_at: "2026-07-12T08:00:00Z",
							}],
						}),
						stderr: "",
						code: 0,
					};
				}
				return {
					stdout: JSON.stringify({
						kind: "nopal.plot_establishment/v1",
						ok: true,
						outcome: "established",
						plot: { plot_id: "plot-1" },
						diagnostics: [],
					}),
					stderr: "",
					code: 0,
				};
			},
			sendUserMessage: async () => {},
			appendEntry() {},
		});
		const ctx = {
			cwd: "/repo/worktree",
			hasUI: true,
			ui: { notify() {}, setStatus() {}, setTitle() {} },
			sessionManager: { getEntries: () => [] },
		};
		await commands.kickoff.handler("TASK-51", ctx);
		for (const handler of handlers.agent_end ?? []) await handler({}, ctx);
	} finally {
		if (previousSignals === undefined) delete process.env.NOPAL_BEISLID_SIGNALS;
		else process.env.NOPAL_BEISLID_SIGNALS = previousSignals;
	}
	assert.ok(calls.some((args) => args.join(" ") === "--json plot establish --event kickoff_context_ready --workspace /repo/worktree"));
});

test("fresh Session establishes identity, starts the bridge, and binds its ready endpoint without restart", async (t) => {
	const extension = await loadNopalModule<{ default: (pi: any) => void }>("../index.js");
	const { defaultSessionSocketPath, SESSION_COMMAND_KIND, SESSION_EVENT_KIND, SESSION_REPLAY_COMPLETE_KIND, SESSION_SUBSCRIBE_KIND } = await loadNopalModule<
		typeof import("../session-bridge.ts")
	>("../session-bridge.js");
	const commands: Record<string, any> = {};
	const handlers: Record<string, Array<(event: unknown, ctx: any) => Promise<void> | void>> = {};
	const establishmentCalls: string[][] = [];
	let pointerReads = 0;
	const plotId = `plot-fresh-${process.pid}`;
	const sessionId = `session-fresh-${process.pid}`;
	const stalePlotId = `plot-stale-${process.pid}`;
	const staleSessionId = `session-stale-${process.pid}`;
	const deliveries: string[] = [];
	const previousPane = process.env.TMUX_PANE;
	const previousSignals = process.env.NOPAL_BEISLID_SIGNALS;
	process.env.TMUX_PANE = "%77";
	process.env.NOPAL_BEISLID_SIGNALS = "0";
	try {
		extension.default({
			registerTool() {},
			registerCommand(name: string, command: any) { commands[name] = command; },
			on(event: string, handler: any) { (handlers[event] ??= []).push(handler); },
			getActiveTools: () => [],
			setActiveTools() {},
			exec: async (command: string, args: string[]) => {
				if (command === "tmux") {
					return {
						stdout: `${args.at(-1) === "@nopal_plot" ? stalePlotId : staleSessionId}\n`,
						stderr: "",
						code: 0,
					};
				}
				if (args.includes("workflow")) {
					return { stdout: JSON.stringify({
						ok: true,
						handoff: { auto: false, events: [], exclude: [] },
						babysit: { token_budget: null },
						establishment: { events: ["kickoff_context_ready"] },
					}), stderr: "", code: 0 };
				}
				if (args.includes("pointer")) {
					pointerReads += 1;
					return { stdout: JSON.stringify({
						ok: true,
						source: ".nopal/checkpoints/latest.json",
						entries: pointerReads === 1 ? [] : [{
							event: "kickoff_context_ready",
							path: "plans/kickoff-context-TASK-54.md",
							written_at: "2026-07-13T00:00:00Z",
						}],
					}), stderr: "", code: 0 };
				}
				if (args.includes("establish")) {
					establishmentCalls.push(args);
					return { stdout: JSON.stringify({
						kind: "nopal.plot_establishment/v1",
						ok: true,
						outcome: establishmentCalls.length === 1 ? "established" : "extended",
						plot: { plot_id: plotId, selected_session_id: sessionId },
						diagnostics: [],
					}), stderr: "", code: 0 };
				}
				return { stdout: "", stderr: "", code: 1 };
			},
			sendUserMessage: async (text: string) => { deliveries.push(text); },
			appendEntry() {},
		});
		const ctx = {
			cwd: "/repo/worktree",
			hasUI: false,
			ui: { notify() {}, setStatus() {}, setTitle() {} },
			sessionManager: { getEntries: () => [], getBranch: () => [] },
		};
		t.after(async () => {
			for (const handler of handlers.session_shutdown ?? []) {
				await handler({ type: "session_shutdown", reason: "test cleanup" }, ctx);
			}
		});
		for (const handler of handlers.session_start ?? []) await handler({ type: "session_start", reason: "startup" }, ctx);
		await commands.kickoff.handler("TASK-54", ctx);
		for (const handler of handlers.agent_end ?? []) await handler({}, ctx);

		assert.equal(establishmentCalls.length, 2);
		assert.equal(establishmentCalls[0].includes("--protocol-address"), false);
		assert.equal(establishmentCalls[1].includes("--protocol-address"), true);
		assert.equal(establishmentCalls[1].at(-2), "--protocol-state");
		assert.equal(establishmentCalls[1].at(-1), "ready");
		const protocolAddressIndex = establishmentCalls[1].indexOf("--protocol-address");
		const protocolAddress = establishmentCalls[1][protocolAddressIndex + 1];
		assert.equal(protocolAddress, defaultSessionSocketPath({ plotId, sessionId }));
		await assert.rejects(lstat(defaultSessionSocketPath({
			plotId: stalePlotId,
			sessionId: staleSessionId,
		})), /ENOENT/);
		const ready = await new Promise<any>((resolve, reject) => {
			const socket = createConnection(protocolAddress);
			let buffer = "";
			let readyEvent: any;
			let commandSent = false;
			socket.once("connect", () => {
				socket.write(`${JSON.stringify({
					kind: SESSION_SUBSCRIBE_KIND,
					request_id: "subscribe-core-identity",
					plot_id: plotId,
					session_id: sessionId,
					after_cursor: null,
					page_limit: 256,
				})}\n`);
			});
			socket.on("data", (chunk) => {
				buffer += chunk.toString("utf8");
				for (;;) {
					const newline = buffer.indexOf("\n");
					if (newline < 0) break;
					const frame = JSON.parse(buffer.slice(0, newline));
					buffer = buffer.slice(newline + 1);
					if (frame.kind === SESSION_EVENT_KIND && frame.event?.type === "session_ready") {
						readyEvent = frame;
					}
					if (frame.kind !== SESSION_REPLAY_COMPLETE_KIND || commandSent || !readyEvent) continue;
					commandSent = true;
					socket.write(`${JSON.stringify({
					kind: SESSION_COMMAND_KIND,
					command_id: "command-core-identity",
					plot_id: plotId,
					session_id: sessionId,
					command: { type: "prompt", text: "accepted by Core identity" },
				})}\n`);
					resolve(readyEvent);
					socket.end();
				}
			});
			socket.on("error", reject);
		});
		assert.equal(ready.plot_id, plotId);
		assert.equal(ready.session_id, sessionId);
		for (let attempt = 0; attempt < 100 && !deliveries.includes("accepted by Core identity"); attempt += 1) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
		assert.ok(deliveries.includes("accepted by Core identity"));
		for (const handler of handlers.session_shutdown ?? []) await handler({ type: "session_shutdown", reason: "quit" }, ctx);
	} finally {
		if (previousPane === undefined) delete process.env.TMUX_PANE;
		else process.env.TMUX_PANE = previousPane;
		if (previousSignals === undefined) delete process.env.NOPAL_BEISLID_SIGNALS;
		else process.env.NOPAL_BEISLID_SIGNALS = previousSignals;
	}
});

test("Nopal extension retries a durable pending establishment after a transient failure", async () => {
	const extension = await loadNopalModule<{ default: (pi: any) => void }>("../index.js");
	const commands: Record<string, any> = {};
	const handlers: Record<string, Array<(event: unknown, ctx: any) => Promise<void> | void>> = {};
	const entries: Array<{ type: string; customType: string; data: unknown }> = [];
	let workflowReads = 0;
	let pointerReads = 0;
	let establishmentAttempts = 0;
	const previousSignals = process.env.NOPAL_BEISLID_SIGNALS;
	process.env.NOPAL_BEISLID_SIGNALS = "0";
	try {
		extension.default({
			registerTool() {},
			registerCommand(name: string, command: any) {
				commands[name] = command;
			},
			on(event: string, handler: any) {
				(handlers[event] ??= []).push(handler);
			},
			exec: async (_command: string, args: string[]) => {
				if (args.includes("workflow")) {
					workflowReads += 1;
					if (workflowReads === 2) {
						return { stdout: "", stderr: "temporary config failure", code: 1 };
					}
					return {
						stdout: JSON.stringify({
							ok: true,
							handoff: { auto: false, events: [], exclude: [] },
							babysit: { token_budget: null },
							establishment: { events: ["kickoff_context_ready"] },
						}),
						stderr: "",
						code: 0,
					};
				}
				if (args.includes("pointer")) {
					pointerReads += 1;
					return {
						stdout: JSON.stringify({
							ok: true,
							source: ".nopal/checkpoints/latest.json",
							entries: pointerReads === 1 ? [] : [{
								event: "kickoff_context_ready",
								path: "plans/kickoff-context-TASK-51.md",
								written_at: "2026-07-12T08:00:00Z",
							}],
						}),
						stderr: "",
						code: 0,
					};
				}
				establishmentAttempts += 1;
				const ok = establishmentAttempts > 1;
				return {
					stdout: JSON.stringify({
						kind: "nopal.plot_establishment/v1",
						ok,
						outcome: ok ? "unchanged" : null,
						plot: ok ? { plot_id: "plot-1" } : null,
						diagnostics: ok ? [] : [{ message: "lock temporarily unavailable" }],
					}),
					stderr: ok ? "" : "temporary failure",
					code: ok ? 0 : 1,
				};
			},
			sendUserMessage: async () => {},
			appendEntry(customType: string, data: unknown) {
				entries.push({ type: "custom", customType, data });
			},
		});
		const ctx = {
			cwd: "/repo/worktree",
			hasUI: false,
			ui: { notify() {}, setStatus() {}, setTitle() {} },
			sessionManager: { getEntries: () => entries },
		};
		await commands.kickoff.handler("TASK-51", ctx);
		for (const handler of handlers.agent_end ?? []) await handler({}, ctx);
		assert.equal(establishmentAttempts, 1);
		assert.deepEqual(entries.map((entry) => entry.customType), ["nopal-plot-establishment-pending"]);

		const recoveryHandler = handlers.session_start?.at(-1);
		assert.ok(recoveryHandler);
		await recoveryHandler({}, ctx);
		assert.equal(establishmentAttempts, 1);
		assert.deepEqual(entries.map((entry) => entry.customType), ["nopal-plot-establishment-pending"]);

		await recoveryHandler({}, ctx);
	} finally {
		if (previousSignals === undefined) delete process.env.NOPAL_BEISLID_SIGNALS;
		else process.env.NOPAL_BEISLID_SIGNALS = previousSignals;
	}
	assert.equal(establishmentAttempts, 2);
	assert.deepEqual(entries.map((entry) => entry.customType), [
		"nopal-plot-establishment-pending",
		"nopal-plot-establishment-applied",
		"nopal-plot-establishment-complete",
	]);
});
