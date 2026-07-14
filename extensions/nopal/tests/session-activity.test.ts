import assert from "node:assert/strict";
import { test } from "node:test";

import type {
	ToolCallEvent,
	ToolResultEvent,
} from "@earendil-works/pi-coding-agent";

import {
	DurableSessionLog,
	MAX_ACTIVITY_OUTPUT_BYTES,
	SESSION_EVENT_ENTRY,
	SESSION_EVENT_V3_KIND,
	type AppendSessionEvent,
	type DurableSessionEvent,
	type PiSessionEntry,
} from "../session-log.ts";
import { loadNopalModule } from "./setup.ts";

const {
	ActivityProductionError,
	SessionActivityProducer,
	boundActivityText,
	registerSessionActivityHooks,
} = await loadNopalModule<typeof import("../session-activity.ts")>("../session-activity.ts");

const binding = { plotId: "plot-01", sessionId: "session-01" };

function producerHarness(existingEvents: readonly DurableSessionEvent[] = []) {
	const inputs: AppendSessionEvent[] = [];
	let monotonic = 100;
	let wall = "2026-07-13T10:00:00.000Z";
	const producer = new SessionActivityProducer({
		binding,
		existingEvents,
		publish(input) {
			inputs.push(structuredClone(input));
		},
		monotonicNow: () => monotonic,
		wallNow: () => wall,
	});
	return {
		producer,
		inputs,
		setMonotonic(value: number) { monotonic = value; },
		setWall(value: string) { wall = value; },
	};
}

test("pins Pi tool_call and tool_result public hook shapes", () => {
	const handlers = new Map<string, Array<(event: unknown) => unknown>>();
	const pi = {
		on(name: string, handler: (event: unknown) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
	};
	const harness = producerHarness();
	registerSessionActivityHooks(pi as never, {
		producer: () => harness.producer,
		commandId: () => "command-01",
	});

	assert.deepEqual([...handlers.keys()], ["tool_call", "tool_result"]);
	const call = {
		type: "tool_call",
		toolCallId: "pi-tool-01",
		toolName: "bash",
		input: { command: "printf hello", timeout: 30 },
	} satisfies ToolCallEvent;
	const result = {
		type: "tool_result",
		toolCallId: "pi-tool-01",
		toolName: "bash",
		input: { command: "printf hello", timeout: 30 },
		content: [{ type: "text", text: "hello" }],
		details: undefined,
		isError: false,
	} satisfies ToolResultEvent;

	handlers.get("tool_call")?.[0]?.(call);
	harness.setMonotonic(112);
	handlers.get("tool_result")?.[0]?.(result);

	assert.deepEqual(harness.inputs.map((input) => input.event.type), [
		"command_started",
		"command_finished",
	]);
	assert.equal(harness.inputs[0]?.commandId, "command-01");
	assert.equal(harness.inputs[1]?.commandId, "command-01");
	assert.equal((harness.inputs[0]?.event as Record<string, unknown>).tool_call_id, "pi-tool-01");
});

test("observes the documented handler order without mutating Pi events or claiming later mutations", () => {
	const handlers = new Map<string, Array<(event: any) => unknown>>();
	const pi = {
		on(name: string, handler: (event: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
	};
	const harness = producerHarness();
	registerSessionActivityHooks(pi as never, { producer: () => harness.producer });
	pi.on("tool_call", (event) => { event.input.command = "later mutation"; });
	pi.on("tool_result", (event) => { event.content[0].text = "later result mutation"; });
	const call = {
		type: "tool_call",
		toolCallId: "pi-tool-order",
		toolName: "bash",
		input: { command: "observed command" },
	};
	const result = {
		type: "tool_result",
		toolCallId: "pi-tool-order",
		toolName: "bash",
		input: { command: "later mutation" },
		content: [{ type: "text", text: "observed output" }],
		details: undefined,
		isError: false,
	};

	for (const handler of handlers.get("tool_call") ?? []) handler(call);
	harness.setMonotonic(125);
	for (const handler of handlers.get("tool_result") ?? []) handler(result);

	assert.equal(call.input.command, "later mutation");
	assert.equal(result.content[0]?.text, "later result mutation");
	assert.equal((harness.inputs[0]?.event as Record<string, unknown>).command, "observed command");
	assert.equal(
		((harness.inputs[1]?.event as Record<string, any>).output as Record<string, unknown>).text,
		"observed output",
	);
	assert.equal(JSON.stringify(harness.inputs).includes("later result mutation"), false);
});

test("Pi hook callbacks swallow producer and diagnostic failures", () => {
	const handlers = new Map<string, Array<(event: unknown, context: unknown) => unknown>>();
	const pi = {
		on(name: string, handler: (event: unknown, context: unknown) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
	};
	const producer = new SessionActivityProducer({
		binding,
		publish() { throw new Error("persistence failed"); },
		monotonicNow: () => 1,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	registerSessionActivityHooks(pi as never, {
		producer: () => producer,
		onError() { throw new Error("diagnostic failed"); },
	});
	assert.doesNotThrow(() => handlers.get("tool_call")?.[0]?.({
		type: "tool_call",
		toolCallId: "diagnostic-failure",
		toolName: "read",
		input: { path: "/repo" },
	}, {}));
});

test("maps one shell lifecycle with stable identities, monotonic duration, and unavailable exit facts", () => {
	const harness = producerHarness();
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "pi-shell-01",
		toolName: "bash",
		input: { command: "pwd" },
	}, "command-01");
	harness.setMonotonic(142);
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "pi-shell-01",
		toolName: "bash",
		input: { command: "pwd" },
		content: [{ type: "text", text: "/repo\n" }],
		details: undefined,
		isError: false,
	});

	assert.equal(harness.inputs.length, 2);
	const start = harness.inputs[0]?.event as Record<string, any>;
	const finish = harness.inputs[1]?.event as Record<string, any>;
	assert.equal(start.type, "command_started");
	assert.equal(finish.type, "command_finished");
	assert.equal(start.activity_id, finish.activity_id);
	assert.equal(start.tool_call_id, "pi-shell-01");
	assert.equal(finish.duration_ms, 42);
	assert.deepEqual(finish.exit, {
		type: "unavailable",
		reason: "Pi tool_result does not expose an exit code or signal",
	});
	assert.equal(finish.outcome, "succeeded");
	assert.equal(finish.output.channel, "combined");
	assert.deepEqual(harness.inputs.map((input) => input.event.type), [
		"command_started",
		"command_finished",
	]);
	assert.match(harness.inputs[0]?.eventId ?? "", /^nopal\.session\.activity-event\/v1:[0-9a-f]{64}$/u);
	assert.notEqual(harness.inputs[0]?.eventId, harness.inputs[1]?.eventId);
});

test("rejects orphan terminals and tool-call identity conflicts before publication", () => {
	const harness = producerHarness();
	assert.throws(() => harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "orphan",
		toolName: "read",
		input: { path: "/tmp/file" },
		content: [{ type: "text", text: "contents" }],
		details: undefined,
		isError: false,
	}), (error) => error instanceof ActivityProductionError && error.code === "orphan_terminal");

	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "conflict",
		toolName: "read",
		input: { path: "/tmp/file" },
	});
	assert.throws(() => harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "conflict",
		toolName: "write",
		input: { path: "/tmp/file", content: "changed" },
		content: [{ type: "text", text: "ok" }],
		details: undefined,
		isError: false,
	}), (error) => error instanceof ActivityProductionError && error.code === "identity_conflict");
	assert.equal(harness.inputs.length, 1);
});

test("redacts credentials before deterministic UTF-8-safe head-tail bounds", () => {
	const source = `HEAD🙂 API_TOKEN=top-secret Bearer abc.def.ghi ${"中".repeat(80)} TAIL🙂`;
	const first = boundActivityText(source, 96);
	const second = boundActivityText(source, 96);

	assert.deepEqual(first, second);
	assert.equal(first.truncated, true);
	assert.equal(Buffer.byteLength(first.text, "utf8") <= 96, true);
	assert.equal(first.original_bytes - first.omitted_bytes, Buffer.byteLength(first.text, "utf8"));
	assert.equal(first.text.includes("top-secret"), false);
	assert.equal(first.text.includes("abc.def.ghi"), false);
	assert.equal(first.text.includes("[REDACTED]"), true);
	assert.equal(first.text.startsWith("HEAD🙂"), true);
	assert.equal(first.text.endsWith("TAIL🙂"), true);
	assert.equal(first.text.includes("�"), false);
});

test("bounds and redacts observed shell command and output before publication", () => {
	const harness = producerHarness();
	const syntheticCredential = [["API", "KEY"].join("_"), ["sk", "1234567890abcdefghijkl"].join("-")].join("=");
	const command = `${syntheticCredential} printf ${"🙂".repeat(3000)}`;
	const output = `HEAD token=secret-value ${"中".repeat(20_000)} TAIL password=hunter2`;
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "bounded-shell",
		toolName: "bash",
		input: { command },
	});
	harness.setMonotonic(120);
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "bounded-shell",
		toolName: "bash",
		input: { command },
		content: [{ type: "text", text: output }],
		details: undefined,
		isError: false,
	});

	const serialized = JSON.stringify(harness.inputs);
	const start = harness.inputs[0]?.event as Record<string, any>;
	const preview = (harness.inputs[1]?.event as Record<string, any>).output as Record<string, any>;
	assert.equal(Buffer.byteLength(start.command, "utf8") <= 8192, true);
	assert.equal(Buffer.byteLength(preview.text, "utf8") <= MAX_ACTIVITY_OUTPUT_BYTES, true);
	assert.equal(preview.truncated, true);
	assert.equal(preview.original_bytes - preview.omitted_bytes, Buffer.byteLength(preview.text, "utf8"));
	assert.equal(preview.text.startsWith("HEAD token=[REDACTED]"), true);
	assert.equal(preview.text.endsWith("TAIL password=[REDACTED]"), true);
	assert.equal(serialized.includes("hunter2"), false);
	assert.equal(serialized.includes("secret-value"), false);
	assert.equal(serialized.includes("sk-1234567890abcdefghijkl"), false);
});

test("redacts credential presentations split across adjacent Pi text parts", () => {
	const harness = producerHarness();
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "split-secret-output",
		toolName: "bash",
		input: { command: "split-output" },
	});
	harness.setMonotonic(115);
	const parts = [
		"small prefix gh",
		"p_1234567890abcdefghijkl Bear",
		"er abc.def.ghi pass",
		"word=hun",
		"ter2 safe tail",
	];
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "split-secret-output",
		toolName: "bash",
		input: { command: "split-output" },
		content: parts.map((text) => ({ type: "text" as const, text })),
		details: undefined,
		isError: false,
	});
	const output = (harness.inputs[1]?.event as Record<string, any>).output as Record<string, any>;
	assert.equal(output.text, "small prefix [REDACTED] Bearer [REDACTED] password=[REDACTED] safe tail");
	assert.equal(output.truncated, false);
	assert.equal(output.original_bytes, Buffer.byteLength(output.text, "utf8"));
	assert.equal(output.omitted_bytes, 0);
	for (const forbidden of ["ghp_1234567890abcdefghijkl", "abc.def.ghi", "hunter2"]) {
		assert.equal(JSON.stringify(harness.inputs).includes(forbidden), false, forbidden);
	}
});

test("renders control characters in command and failure display fields without rejecting the activity", () => {
	const persisted: DurableSessionEvent[] = [];
	const log = DurableSessionLog.hydrate({
		binding,
		activeBranch: [],
		appendKind: SESSION_EVENT_V3_KIND,
		appendEntry(_type, event) { persisted.push(event); },
	});
	const producer = new SessionActivityProducer({
		binding,
		publish: (input) => log.append(input),
		monotonicNow: () => 100,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	producer.observeToolCall({
		type: "tool_call",
		toolCallId: "multiline-shell",
		toolName: "bash",
		input: { command: "printf one\nprintf two\t# tab" },
	});
	producer.observeToolResult({
		type: "tool_result",
		toolCallId: "multiline-shell",
		toolName: "bash",
		input: { command: "printf one\nprintf two\t# tab" },
		content: [{ type: "text", text: "failure line one\nfailure line two" }],
		details: undefined,
		isError: true,
	});
	assert.equal((persisted[0]?.event as Record<string, any>).command, "printf one\\nprintf two\\t# tab");
	assert.equal((persisted[1]?.event as Record<string, any>).message, "failure line one\\nfailure line two");
});

test("allowlists known tool summaries without persisting file content or result payloads", () => {
	const harness = producerHarness();
	const rawContent = "PRIVATE_BODY token=do-not-store";
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "write-01",
		toolName: "write",
		input: { path: "/repo/note.txt", content: rawContent },
	});
	harness.setMonotonic(130);
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "write-01",
		toolName: "write",
		input: { path: "/repo/note.txt", content: rawContent },
		content: [{ type: "text", text: "wrote PRIVATE_RESULT" }],
		details: undefined,
		isError: false,
	});

	const serialized = JSON.stringify(harness.inputs);
	const start = harness.inputs[0]?.event as Record<string, any>;
	const finish = harness.inputs[1]?.event as Record<string, any>;
	assert.equal(start.summary.text, `Write /repo/note.txt (${Buffer.byteLength(rawContent)} bytes)`);
	assert.equal(finish.summary.text, "Write completed");
	for (const forbidden of ["PRIVATE_BODY", "do-not-store", "PRIVATE_RESULT", "input", "arguments", "result", "details"]) {
		assert.equal(serialized.includes(forbidden), false, forbidden);
	}
});

test("unknown tools expose only an explicit unavailable summary", () => {
	const harness = producerHarness();
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "custom-01",
		toolName: "deploy_private",
		input: { apiToken: "raw-argument-secret", target: "production" },
	});
	harness.setMonotonic(150);
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "custom-01",
		toolName: "deploy_private",
		input: { apiToken: "raw-argument-secret", target: "production" },
		content: [{ type: "text", text: "raw result secret" }],
		details: { authorization: "raw-details-secret" },
		isError: false,
	});

	const start = harness.inputs[0]?.event as Record<string, any>;
	const finish = harness.inputs[1]?.event as Record<string, any>;
	assert.deepEqual(start.summary, {
		text: "Details unavailable",
		truncated: false,
		original_bytes: 19,
		omitted_bytes: 0,
		details_unavailable: true,
	});
	assert.deepEqual(finish.summary, start.summary);
	const serialized = JSON.stringify(harness.inputs);
	for (const forbidden of ["raw-argument-secret", "production", "raw result secret", "raw-details-secret", "apiToken", "authorization"]) {
		assert.equal(serialized.includes(forbidden), false, forbidden);
	}
});

test("duplicate starts and terminals suppress exactly while conflicting outcomes fail", () => {
	const harness = producerHarness();
	const call = {
		type: "tool_call" as const,
		toolCallId: "duplicate-01",
		toolName: "read" as const,
		input: { path: "/repo/file" },
	};
	const result = {
		type: "tool_result" as const,
		toolCallId: "duplicate-01",
		toolName: "read" as const,
		input: { path: "/repo/file" },
		content: [{ type: "text" as const, text: "contents" }],
		details: undefined,
		isError: false,
	};
	harness.producer.observeToolCall(call);
	harness.producer.observeToolCall(structuredClone(call));
	harness.setMonotonic(125);
	harness.producer.observeToolResult(result);
	harness.producer.observeToolResult(structuredClone(result));
	assert.equal(harness.inputs.length, 2);
	assert.throws(() => harness.producer.observeToolResult({ ...result, isError: true }), (error) =>
		error instanceof ActivityProductionError && error.code === "terminal_conflict");
	assert.equal(harness.inputs.length, 2);

	const shell = {
		type: "tool_call" as const,
		toolCallId: "duplicate-shell",
		toolName: "bash" as const,
		input: { command: "printf result" },
	};
	harness.producer.observeToolCall(shell);
	const shellResult = {
		type: "tool_result" as const,
		toolCallId: "duplicate-shell",
		toolName: "bash" as const,
		input: { command: "printf result" },
		content: [{ type: "text" as const, text: "one" }],
		details: undefined,
		isError: false,
	};
	harness.producer.observeToolResult(shellResult);
	assert.throws(() => harness.producer.observeToolResult({
		...shellResult,
		content: [{ type: "text", text: "different output" }],
	}), (error) => error instanceof ActivityProductionError && error.code === "terminal_conflict");
	harness.setMonotonic(126);
	assert.doesNotThrow(() => harness.producer.observeToolResult(shellResult));
	assert.equal(harness.inputs.length, 4);
	assert.equal((harness.inputs[3]?.event as Record<string, any>).duration_ms, 0);
});

test("streams a multi-megabyte many-part shell result into one bounded preview", () => {
	const harness = producerHarness();
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "large-output",
		toolName: "bash",
		input: { command: "large-output" },
	});
	harness.setMonotonic(110);
	const part = `${"🙂".repeat(256)} token=part-secret`;
	const content = Array.from({ length: 384 }, (_, index) => ({
		type: "text" as const,
		text: index === 0
			? "HEAD small prefix"
			: index === 1
				? `${"中".repeat(1_100_000)} token=large-secret`
				: index === 383 ? `${part} TAIL` : part,
	}));
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "large-output",
		toolName: "bash",
		input: { command: "large-output" },
		content,
		details: undefined,
		isError: false,
	});
	const output = (harness.inputs[1]?.event as Record<string, any>).output as Record<string, any>;
	assert.equal(output.truncated, true);
	assert.equal(Buffer.byteLength(output.text, "utf8") <= MAX_ACTIVITY_OUTPUT_BYTES, true);
	assert.equal(output.original_bytes > 3 * 1024 * 1024, true);
	assert.equal(output.original_bytes - output.omitted_bytes, Buffer.byteLength(output.text, "utf8"));
	assert.equal(output.text.startsWith("HEAD "), true);
	assert.equal(output.text.endsWith(" TAIL"), true);
	assert.equal(output.text.includes("part-secret"), false);
	assert.equal(output.text.includes("large-secret"), false);
	assert.equal(output.text.includes("�"), false);
});

test("formats failure labels from a whole Unicode scalar", () => {
	const harness = producerHarness();
	harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "unicode-tool",
		toolName: "🙂tool",
		input: {},
	});
	harness.setMonotonic(105);
	harness.producer.observeToolResult({
		type: "tool_result",
		toolCallId: "unicode-tool",
		toolName: "🙂tool",
		input: {},
		content: [{ type: "text", text: "private failure detail" }],
		details: undefined,
		isError: true,
	});
	const failure = harness.inputs[1]?.event as Record<string, any>;
	assert.equal(failure.message, "🙂tool failed");
	assert.equal(failure.message.includes("�"), false);
});

test("failed publication is transactional at both lifecycle boundaries", () => {
	let failStart = true;
	const published: AppendSessionEvent[] = [];
	const producer = new SessionActivityProducer({
		binding,
		publish(input) {
			if (failStart) throw new Error("disk full");
			published.push(structuredClone(input));
		},
		monotonicNow: () => 100,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	const call = {
		type: "tool_call" as const,
		toolCallId: "persistence-start",
		toolName: "read" as const,
		input: { path: "/repo/file" },
	};
	assert.throws(() => producer.observeToolCall(call), /disk full/u);
	assert.throws(() => producer.observeToolResult({
		type: "tool_result",
		toolCallId: "persistence-start",
		toolName: "read",
		input: { path: "/repo/file" },
		content: [],
		details: undefined,
		isError: false,
	}), (error) => error instanceof ActivityProductionError && error.code === "orphan_terminal");

	failStart = false;
	producer.observeToolCall(call);
	let failTerminal = true;
	const terminalProducer = new SessionActivityProducer({
		binding,
		publish(input) {
			if (input.event.type !== "tool_started" && failTerminal) throw new Error("terminal disk full");
			published.push(structuredClone(input));
		},
		monotonicNow: () => 200,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	terminalProducer.observeToolCall({ ...call, toolCallId: "persistence-terminal" });
	const terminal = {
		type: "tool_result" as const,
		toolCallId: "persistence-terminal",
		toolName: "read" as const,
		input: { path: "/repo/file" },
		content: [],
		details: undefined,
		isError: false,
	};
	assert.throws(() => terminalProducer.observeToolResult(terminal), /terminal disk full/u);
	failTerminal = false;
	assert.throws(() => terminalProducer.observeToolResult(terminal), (error) =>
		error instanceof ActivityProductionError && error.code === "orphan_terminal");
});

test("restart suppresses complete duplicates and leaves an interrupted start incomplete", () => {
	const persisted: DurableSessionEvent[] = [];
	const log = DurableSessionLog.hydrate({
		binding,
		activeBranch: [],
		appendKind: SESSION_EVENT_V3_KIND,
		appendEntry(_type, event) { persisted.push(event); },
	});
	const first = new SessionActivityProducer({
		binding,
		publish: (input) => log.append(input),
		monotonicNow: () => 100,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	const completeCall = {
		type: "tool_call" as const,
		toolCallId: "restart-complete",
		toolName: "ls" as const,
		input: { path: "/repo" },
	};
	first.observeToolCall(completeCall);
	first.observeToolResult({
		type: "tool_result",
		toolCallId: "restart-complete",
		toolName: "ls",
		input: { path: "/repo" },
		content: [{ type: "text", text: "file" }],
		details: undefined,
		isError: false,
	});
	first.observeToolCall({
		type: "tool_call",
		toolCallId: "restart-interrupted",
		toolName: "grep",
		input: { pattern: "needle", path: "/repo" },
	});
	const branch = persisted.map((event, index): PiSessionEntry => ({
		type: "custom",
		id: `activity-${index}`,
		parentId: index === 0 ? null : `activity-${index - 1}`,
		customType: SESSION_EVENT_ENTRY,
		data: event,
	}));
	const restartedLog = DurableSessionLog.hydrate({
		binding,
		activeBranch: branch,
		appendKind: SESSION_EVENT_V3_KIND,
		appendEntry(_type, event) { persisted.push(event); },
	});
	const restarted = new SessionActivityProducer({
		binding,
		existingEvents: restartedLog.events(),
		publish: (input) => restartedLog.append(input),
		monotonicNow: () => 500,
		wallNow: () => "2026-07-13T11:00:00.000Z",
	});
	const count = persisted.length;
	restarted.observeToolCall(completeCall);
	restarted.observeToolResult({
		type: "tool_result",
		toolCallId: "restart-complete",
		toolName: "ls",
		input: { path: "/repo" },
		content: [],
		details: undefined,
		isError: false,
	});
	assert.equal(persisted.length, count);
	assert.throws(() => restarted.observeToolResult({
		type: "tool_result",
		toolCallId: "restart-interrupted",
		toolName: "grep",
		input: { pattern: "needle", path: "/repo" },
		content: [],
		details: undefined,
		isError: false,
	}), (error) => error instanceof ActivityProductionError && error.code === "duration_unavailable");
	assert.equal(persisted.length, count);
});

test("rejects unsafe identities and invalid clocks before publication", () => {
	const harness = producerHarness();
	assert.throws(() => harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "unsafe\ncall",
		toolName: "read",
		input: { path: "/repo" },
	}), (error) => error instanceof ActivityProductionError && error.code === "invalid_identity");
	assert.throws(() => harness.producer.observeToolCall({
		type: "tool_call",
		toolCallId: "oversized-tool-name",
		toolName: "x".repeat(257),
		input: {},
	}), (error) => error instanceof ActivityProductionError && error.code === "invalid_identity");
	const invalidClock = new SessionActivityProducer({
		binding,
		publish: () => assert.fail("invalid clock must not publish"),
		monotonicNow: () => Number.NaN,
		wallNow: () => "2026-07-13T10:00:00.000Z",
	});
	assert.throws(() => invalidClock.observeToolCall({
		type: "tool_call",
		toolCallId: "clock",
		toolName: "read",
		input: { path: "/repo" },
	}), (error) => error instanceof ActivityProductionError && error.code === "invalid_clock");
	const invalidWall = new SessionActivityProducer({
		binding,
		publish: () => assert.fail("invalid wall clock must not publish"),
		monotonicNow: () => 1,
		wallNow: () => "not-a-timestamp",
	});
	assert.throws(() => invalidWall.observeToolCall({
		type: "tool_call",
		toolCallId: "wall-clock",
		toolName: "read",
		input: { path: "/repo" },
	}), (error) => error instanceof ActivityProductionError && error.code === "invalid_clock");
});
