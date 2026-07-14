import assert from "node:assert/strict";
import { chmod, lstat, mkdir, readFile, writeFile } from "node:fs/promises";
import { createConnection } from "node:net";
import { join } from "node:path";
import { test } from "node:test";

import type { DurableSessionEvent, PiSessionEntry } from "../session-log.ts";
import type { SessionEvent } from "../session-bridge.ts";
import { loadNopalModule } from "./setup.ts";

const {
	MAX_JSONL_LINE_BYTES,
	MAX_KNOWN_SESSION_CURSORS,
	MAX_SESSION_IDENTITY_BYTES,
	SESSION_COMMAND_KIND,
	SESSION_ENDPOINT_KIND,
	SESSION_EVENT_ENTRY,
	SESSION_EVENT_KIND,
	SESSION_FEED_ERROR_KIND,
	SESSION_MODEL_ERROR_KIND,
	SESSION_MODEL_REQUEST_KIND,
	SESSION_MODEL_STATE_KIND,
	SESSION_REPLAY_COMPLETE_KIND,
	SESSION_SUBSCRIBE_KIND,
	JsonlDecoder,
	NopalSessionBridge,
	SessionFeedConnection,
	SessionModelController,
	SessionProtocolEngine,
	defaultSessionSocketPath,
	parseCommand,
	registerNopalSessionBridge,
	retainRecentSessionCursors,
} = await loadNopalModule<typeof import("../session-bridge.ts")>("../session-bridge.ts");

const binding = { plotId: "plot-01", sessionId: "session-01" };

function durableBranch(events: readonly DurableSessionEvent[]): PiSessionEntry[] {
	return events.map((event, index) => ({
		type: "custom",
		id: `durable-${index + 1}`,
		parentId: index === 0 ? null : `durable-${index}`,
		customType: SESSION_EVENT_ENTRY,
		data: event,
	}));
}

function subscribe(afterCursor: string | null = null, pageLimit = 256) {
	return {
		kind: SESSION_SUBSCRIBE_KIND,
		request_id: "request-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		after_cursor: afterCursor,
		page_limit: pageLimit,
	};
}

function feedHarness(
	engine: InstanceType<typeof SessionProtocolEngine>,
	replayYield?: () => void | Promise<void>,
	sendOverride?: (frame: Record<string, any>) => void | Promise<void>,
	modelController?: InstanceType<typeof SessionModelController>,
) {
	const frames: Array<Record<string, any>> = [];
	let closes = 0;
	const feed = new SessionFeedConnection({
		engine,
		modelController,
		send(frame) {
			if (sendOverride) return sendOverride(frame);
			frames.push(frame);
		},
		close() { closes += 1; },
		replayYield,
	});
	return { feed, frames, closeCount: () => closes };
}

function harness() {
	const entries: Array<{ customType: string; data: unknown }> = [];
	const sent: string[] = [];
	const emitted: SessionEvent[] = [];
	const deferred: Array<() => unknown> = [];
	const ids = Array.from({ length: 256 }, (_, index) =>
		`00000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
	);
	const engine = new SessionProtocolEngine(binding, {
		appendEntry(customType, data) {
			entries.push({ customType, data });
		},
		async sendUserMessage(text) {
			sent.push(text);
		},
		emit(event) {
			emitted.push(event);
		},
		nextId() {
			const id = ids.shift();
			assert.ok(id, "test UUID fixture exhausted");
			return id;
		},
		defer(task) {
			deferred.push(task);
		},
	});
	return {
		engine,
		entries,
		sent,
		emitted,
		async flushDeferred() {
			while (deferred.length > 0) await deferred.shift()?.();
		},
	};
}

test("engine emits and persists one stable ready event", () => {
	const { engine, entries, emitted } = harness();
	const ready = engine.start();

	assert.equal(ready.kind, SESSION_EVENT_KIND);
	assert.equal(ready.event_id, "00000000-0000-4000-8000-000000000001");
	assert.equal(ready.plot_id, "plot-01");
	assert.equal(ready.session_id, "session-01");
	assert.deepEqual(ready.event, { type: "session_ready" });
	assert.equal(ready.sequence, 1);
	assert.equal(ready.previous_cursor, null);
	assert.match(ready.stream_id, /^nopal\.session\.stream\/v1:[0-9a-f]{64}$/u);
	assert.deepEqual(emitted, [ready]);
	assert.deepEqual(entries, [{ customType: SESSION_EVENT_ENTRY, data: ready }]);
	assert.equal(engine.start(), ready, "start must be idempotent");
	assert.equal(entries.length, 1);
});

test("model controller refreshes Pi choices and confirms one exact idle switch", async () => {
	const models = [
		{ provider: "nopal-proof", id: "deterministic-a", name: "Model A" },
		{ provider: "nopal-proof", id: "deterministic-b", name: "Model B" },
	];
	let current = models[0];
	let idle = true;
	let switches = 0;
	const controller = new SessionModelController(binding, {
		available: () => models,
		current: () => current,
		isIdle: () => idle,
		async setModel(model) {
			switches += 1;
			current = model;
			return true;
		},
	});

	const refreshed = await controller.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "refresh-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: { type: "refresh" },
	});
	assert.equal(refreshed.kind, SESSION_MODEL_STATE_KIND);
	assert.equal(refreshed.request_id, "refresh-01");
	assert.equal(refreshed.available.length, 2);

	const switched = await controller.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "switch-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: {
			type: "switch",
			model: { provider: "nopal-proof", id: "deterministic-b" },
		},
	});
	assert.equal(switched.kind, SESSION_MODEL_STATE_KIND);
	assert.equal(switched.current?.id, "deterministic-b");
	const duplicate = await controller.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "switch-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: {
			type: "switch",
			model: { provider: "nopal-proof", id: "deterministic-b" },
		},
	});
	assert.deepEqual(duplicate, switched);
	assert.equal(switches, 1, "an exact duplicate must not call Pi again");
	const conflict = await controller.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "switch-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: {
			type: "switch",
			model: { provider: "nopal-proof", id: "deterministic-a" },
		},
	});
	assert.equal(conflict.kind, SESSION_MODEL_ERROR_KIND);
	assert.equal(conflict.code, "conflict");
	assert.equal(switches, 1);

	idle = false;
	const rejected = await controller.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "switch-busy",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: {
			type: "switch",
			model: { provider: "nopal-proof", id: "deterministic-a" },
		},
	});
	assert.equal(rejected.kind, SESSION_MODEL_ERROR_KIND);
	assert.equal(rejected.code, "busy");
});

test("shared identity fixtures freeze the 4096-byte wire boundary before journal access", async () => {
	const lines = (await readFile(
		join(process.cwd(), "conformance/surface/session/identity-bounds-v1.jsonl"),
		"utf8",
	)).trimEnd().split("\n");
	const atLimit = JSON.parse(lines[0] ?? "null");
	const beyondLimit = JSON.parse(lines[1] ?? "null");

	assert.equal(Buffer.byteLength(atLimit.command_id, "utf8"), MAX_SESSION_IDENTITY_BYTES);
	assert.equal(Buffer.byteLength(beyondLimit.command_id, "utf8"), MAX_SESSION_IDENTITY_BYTES + 1);
	assert.ok(parseCommand(atLimit));
	assert.equal(parseCommand(beyondLimit), undefined);

	const { engine } = harness();
	engine.start();
	assert.equal((await engine.accept(atLimit)).kind, "accepted");
	const rejected = await engine.accept(beyondLimit);
	assert.equal(rejected.kind, "error");
	if (rejected.kind === "error") assert.equal(rejected.error.code, "protocol_violation");
});

test("recent Session cursor retention is bounded and keeps the active branch hottest", () => {
	assert.equal(MAX_KNOWN_SESSION_CURSORS, 100_000);
	const known = new Set(["old-abandoned", "active-common", "newer-abandoned"]);

	retainRecentSessionCursors(known, ["active-common", "active-head"], 2);

	assert.deepEqual([...known], ["active-common", "active-head"]);
});

test("accepted prompt records a typed user event and calls Pi exactly once", async () => {
	const { engine, entries, sent, emitted } = harness();
	engine.start();

	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "Inspect the failing test" },
		future_field: true,
	});

	assert.deepEqual(sent, ["Inspect the failing test"]);
	assert.equal(emitted[1]?.kind, SESSION_EVENT_KIND);
	assert.equal(emitted[1]?.event_id, "00000000-0000-4000-8000-000000000002");
	assert.equal(emitted[1]?.command_id, "command-01");
	assert.equal(emitted[1]?.sequence, 2);
	assert.deepEqual(emitted[1]?.event, { type: "user_message", text: "Inspect the failing test" });
	assert.deepEqual(entries.at(-1), { customType: SESSION_EVENT_ENTRY, data: emitted[1] });

	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "Inspect the failing test" },
	});
	assert.deepEqual(sent, ["Inspect the failing test"], "a repeated command id must not be delivered twice");
	assert.equal(emitted.length, 2, "an exact command retry must not create a semantic event");
});

test("identity and command validation fail closed without calling Pi", async () => {
	const { engine, sent, emitted } = harness();
	engine.start();

	const foreign = await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-foreign",
		plot_id: "plot-02",
		session_id: "session-01",
		command: { type: "prompt", text: "wrong Plot" },
	});
	const invalid = await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-empty",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "  " },
	});

	assert.deepEqual(sent, []);
	assert.equal(emitted.length, 1, "wire errors are operational feed errors, not semantic history");
	assert.equal(foreign.kind === "error" ? foreign.error.code : undefined, "foreign_session");
	assert.equal(invalid.kind === "error" ? invalid.error.code : undefined, "protocol_violation");
});

test("a complete Terminal turn never changes durable history or the structured feed", async () => {
	const { engine, entries, emitted } = harness();
	engine.start();
	const feed = feedHarness(engine);
	await feed.feed.accept(subscribe());
	feed.frames.length = 0;
	const durableBefore = [...engine.log.events()];

	engine.observeInput("typed directly in Terminal", "interactive");
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Terminal reply" }] });
	engine.observeAssistant({ role: "assistant", content: [], errorMessage: "Terminal-local error" });
	engine.observeAgentEnd();

	assert.deepEqual(engine.log.events(), durableBefore);
	assert.equal(entries.length, 1);
	assert.equal(emitted.length, 1);
	assert.deepEqual(feed.frames, []);
});

test("final assistant text becomes one typed event causally linked to the prompt", async () => {
	const { engine, emitted } = harness();
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "answer" },
	});
	engine.observeAgentStart();

	engine.observeAssistant({
		role: "assistant",
		content: [{ type: "toolCall", id: "tool-only", name: "read", arguments: {} }],
	});
	engine.observeAssistant({
		role: "assistant",
		content: [
			{ type: "thinking", thinking: "private" },
			{ type: "text", text: "First" },
			{ type: "toolCall", id: "tool-1", name: "read", arguments: {} },
			{ type: "text", text: " line" },
		],
	});
	engine.observeAgentEnd();

	assert.equal(emitted.at(-1)?.kind, SESSION_EVENT_KIND);
	assert.equal(emitted.at(-1)?.event_id, "00000000-0000-4000-8000-000000000003");
	assert.equal(emitted.at(-1)?.command_id, "command-01");
	assert.equal(emitted.at(-1)?.sequence, 3);
	assert.deepEqual(emitted.at(-1)?.event, { type: "assistant_message", text: "First line" });
});

test("one tool-using agent loop accumulates all visible assistant messages before advancing turns", async () => {
	const { engine, emitted, flushDeferred } = harness();
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "first turn" },
	});
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-02",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "queued follow-up" },
	});

	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "I will inspect it." }] });
	engine.observeAssistant({
		role: "assistant",
		content: [
			{ type: "toolCall", id: "tool-1", name: "read", arguments: {} },
			{ type: "text", text: "The issue is fixed." },
		],
	});
	engine.observeAgentEnd();
	await flushDeferred();
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Second turn reply" }] });
	engine.observeAgentEnd();

	const assistants = emitted.filter((event) => event.event.type === "assistant_message");
	assert.deepEqual(assistants.map((event) => ({ commandId: event.command_id, event: event.event })), [
		{
			commandId: "command-01",
			event: { type: "assistant_message", text: "I will inspect it.\n\nThe issue is fixed." },
		},
		{
			commandId: "command-02",
			event: { type: "assistant_message", text: "Second turn reply" },
		},
	]);
});

test("three rapid structured commands deliver one per outer agent loop without command-id leakage", async () => {
	const { engine, sent, emitted, flushDeferred } = harness();
	engine.start();
	await Promise.all([
		engine.accept({
			kind: SESSION_COMMAND_KIND,
			command_id: "command-01",
			plot_id: "plot-01",
			session_id: "session-01",
			command: { type: "prompt", text: "first" },
		}),
		engine.accept({
			kind: SESSION_COMMAND_KIND,
			command_id: "command-02",
			plot_id: "plot-01",
			session_id: "session-01",
			command: { type: "prompt", text: "second" },
		}),
		engine.accept({
			kind: SESSION_COMMAND_KIND,
			command_id: "command-03",
			plot_id: "plot-01",
			session_id: "session-01",
			command: { type: "prompt", text: "third" },
		}),
	]);
	assert.deepEqual(sent, ["first"]);

	for (const [index, reply] of ["reply one", "reply two", "reply three"].entries()) {
		engine.observeAgentStart();
		engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: reply }] });
		engine.observeAgentEnd();
		if (index < 2) {
			assert.equal(sent.length, index + 1, "next command must wait for deferred post-loop delivery");
			await flushDeferred();
			assert.equal(sent.length, index + 2);
		}
	}

	const assistants = emitted.filter((event) => event.event.type === "assistant_message");
	assert.deepEqual(assistants.map((event) => event.command_id), [
		"command-01",
		"command-02",
		"command-03",
	]);
});

test("closing the protocol engine cancels deferred queued delivery", async () => {
	const { engine, sent, flushDeferred } = harness();
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "first" },
	});
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-02",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "must be cancelled" },
	});
	engine.observeAgentStart();
	engine.observeAgentEnd();
	engine.close();
	await flushDeferred();

	assert.deepEqual(sent, ["first"]);
});

test("a delayed delivery rejection after close emits no late error or queued delivery", async () => {
	const sent: string[] = [];
	const emitted: SessionEvent[] = [];
	let rejectDelivery!: (error: Error) => void;
	const heldDelivery = new Promise<void>((_resolve, reject) => { rejectDelivery = reject; });
	const engine = new SessionProtocolEngine(binding, {
		appendEntry() {},
		async sendUserMessage(text) {
			sent.push(text);
			await heldDelivery;
		},
		emit(event) { emitted.push(event); },
		nextId: () => crypto.randomUUID(),
		defer: (task) => { void task(); },
	});
	engine.start();
	const first = engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-in-flight",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "in flight" },
	});
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-queued",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "must never deliver" },
	});
	engine.close();
	rejectDelivery(new Error("late rejection"));
	await first;

	assert.deepEqual(sent, ["in flight"]);
	assert.equal(emitted.some((event) => event.event.type === "session_error"), false);
});

test("agent-loop errors preserve visible text and advance correlation only once", async () => {
	const { engine, emitted, flushDeferred } = harness();
	engine.start();
	for (const [commandId, text] of [["command-01", "first"], ["command-02", "second"]]) {
		await engine.accept({
			kind: SESSION_COMMAND_KIND,
			command_id: commandId,
			plot_id: "plot-01",
			session_id: "session-01",
			command: { type: "prompt", text },
		});
	}

	engine.observeAgentStart();
	engine.observeAssistant({
		role: "assistant",
		content: [{ type: "text", text: "Partial but useful output" }],
		errorMessage: "tool execution failed",
	});
	engine.observeAgentEnd();
	await flushDeferred();
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Second reply" }] });
	engine.observeAgentEnd();

	const completions = emitted.filter((event) =>
		event.event.type === "assistant_message" || event.event.type === "session_error"
	);
	assert.deepEqual(completions.map((event) => ({ commandId: event.command_id, event: event.event })), [
		{
			commandId: "command-01",
			event: { type: "assistant_message", text: "Partial but useful output" },
		},
		{
			commandId: "command-01",
			event: { type: "session_error", message: "tool execution failed" },
		},
		{
			commandId: "command-02",
			event: { type: "assistant_message", text: "Second reply" },
		},
	]);
});

test("command parser preserves additive envelope and prompt fields", () => {
	const parsed = parseCommand({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		future_envelope_fact: { version: 2 },
		command: {
			type: "prompt",
			text: "inspect",
			future_prompt_fact: true,
		},
	});

	assert.deepEqual(parsed, {
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		future_envelope_fact: { version: 2 },
		command: {
			type: "prompt",
			text: "inspect",
			future_prompt_fact: true,
		},
	});
});

test("command identities follow the shared non-whitespace and no-control contract", () => {
	const command = (commandId: string) => ({
		kind: SESSION_COMMAND_KIND,
		command_id: commandId,
		plot_id: " plot with surrounding spaces ",
		session_id: "session-01",
		command: { type: "prompt", text: "inspect" },
	});

	assert.equal(parseCommand(command("x".repeat(2048)))?.command_id.length, 2048);
	assert.equal(parseCommand(command("command 01"))?.plot_id, " plot with surrounding spaces ");
	assert.equal(parseCommand(command("   ")), undefined);
	assert.equal(parseCommand(command("command\u0000bad")), undefined);
});

test("Terminal input during a structured agent loop is steering, not a queued turn", async () => {
	const { engine, emitted } = harness();
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-structured",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "from Composer" },
	});
	engine.observeAgentStart();
	engine.observeInput("from Terminal", "interactive");

	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "structured reply" }] });
	engine.observeAgentEnd();
	engine.observeInput("new Terminal turn", "interactive");
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Terminal reply" }] });
	engine.observeAgentEnd();

	const assistants = emitted.filter((event) => event.event.type === "assistant_message");
	assert.equal(assistants[0]?.command_id, "command-structured");
	assert.equal(assistants.length, 1);
});

test("Terminal then structured turns do not lend the Terminal reply a command id", async () => {
	const { engine, emitted, flushDeferred } = harness();
	engine.start();
	engine.observeInput("from Terminal", "interactive");
	engine.observeAgentStart();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-structured",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "from Composer" },
	});

	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Terminal reply" }] });
	engine.observeAgentEnd();
	await flushDeferred();
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "structured reply" }] });
	engine.observeAgentEnd();

	const assistants = emitted.filter((event) => event.event.type === "assistant_message");
	assert.equal(assistants.length, 1);
	assert.equal(assistants[0]?.command_id, "command-structured");
});

test("Pi delivery failure emits a persisted Session error", async () => {
	const entries: unknown[] = [];
	const emitted: SessionEvent[] = [];
	const engine = new SessionProtocolEngine(binding, {
		appendEntry(_type, data) { entries.push(data); },
		async sendUserMessage() { throw new Error("Pi is unavailable"); },
		emit(event) { emitted.push(event); },
		nextId: () => crypto.randomUUID(),
	});
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "answer" },
	});

	assert.equal(emitted.at(-1)?.event.type, "session_error");
	assert.equal(entries.at(-1), emitted.at(-1));
});

test("a failed structured delivery advances the FIFO without leaking its command id", async () => {
	const sent: string[] = [];
	const emitted: SessionEvent[] = [];
	let rejectFirst!: (error: Error) => void;
	const firstDelivery = new Promise<void>((_resolve, reject) => { rejectFirst = reject; });
	const engine = new SessionProtocolEngine(binding, {
		appendEntry() {},
		async sendUserMessage(text) {
			sent.push(text);
			if (text === "first fails") await firstDelivery;
		},
		emit(event) { emitted.push(event); },
		nextId: () => crypto.randomUUID(),
		defer: (task) => { void task(); },
	});
	engine.start();
	const first = engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-failed",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "first fails" },
	});
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-next",
		plot_id: "plot-01",
		session_id: "session-01",
		command: { type: "prompt", text: "second succeeds" },
	});
	assert.deepEqual(sent, ["first fails"]);

	rejectFirst(new Error("delivery rejected"));
	await first;
	assert.deepEqual(sent, ["first fails", "second succeeds"]);
	const failure = emitted.find((event) => event.event.type === "session_error");
	assert.equal(failure?.command_id, "command-failed");

	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "next reply" }] });
	engine.observeAgentEnd();
	const assistant = emitted.find((event) => event.event.type === "assistant_message");
	assert.equal(assistant?.command_id, "command-next");
});

test("strict LF decoder handles chunks and rejects oversized or CRLF records", () => {
	const records: unknown[] = [];
	const errors: string[] = [];
	const decoder = new JsonlDecoder((record) => records.push(record), (message) => errors.push(message));
	decoder.push(Buffer.from('{"one":1}\n{"two"'));
	decoder.push(Buffer.from(':2}\n'));
	decoder.push(Buffer.from('{"three":3}\r\n'));
	decoder.push(Buffer.alloc(MAX_JSONL_LINE_BYTES + 1, 0x61));

	assert.deepEqual(records, [{ one: 1 }, { two: 2 }]);
	assert.match(errors[0] ?? "", /LF framing/i);
	assert.match(errors[1] ?? "", /1 MiB/i);
});

test("strict LF decoder rejects invalid UTF-8 instead of replacement-decoding it", () => {
	const records: unknown[] = [];
	const errors: string[] = [];
	const decoder = new JsonlDecoder((record) => records.push(record), (message) => errors.push(message));

	decoder.push(Buffer.from([0x7b, 0x22, 0x78, 0x22, 0x3a, 0x22, 0xc3, 0x28, 0x22, 0x7d, 0x0a]));

	assert.deepEqual(records, []);
	assert.match(errors[0] ?? "", /UTF-8/i);
});

test("oversized JSONL records are discarded through LF before decoding resumes", () => {
	const records: unknown[] = [];
	const errors: string[] = [];
	const decoder = new JsonlDecoder((record) => records.push(record), (message) => errors.push(message));

	decoder.push(Buffer.alloc(MAX_JSONL_LINE_BYTES + 1, 0x61));
	decoder.push(Buffer.from('{"forged_suffix":true}\n{"valid":1}\n'));

	assert.deepEqual(records, [{ valid: 1 }]);
	assert.equal(errors.length, 1);
	assert.match(errors[0] ?? "", /1 MiB/i);
});

test("Unix bridge creates a user-only socket, streams ready, and removes only its owned socket", async (t) => {
	const root = join("/tmp", `nopal-session-bridge-test-${process.pid}-${Date.now()}`);
	await mkdir(root, { recursive: true, mode: 0o700 });
	await chmod(root, 0o700);
	const path = join(root, "bridge.sock");
	const { engine, sent } = harness();
	const bridge = new NopalSessionBridge({ path, engine });
	await bridge.start();
	t.after(async () => { await bridge.close(); });

	assert.equal((await lstat(path)).mode & 0o777, 0o600);
	assert.equal((await lstat(root)).mode & 0o777, 0o700);
	const firstLine = await new Promise<string>((resolve, reject) => {
		const socket = createConnection(path);
		let buffer = "";
		let readyLine: string | undefined;
		socket.on("connect", () => {
			socket.write(`${JSON.stringify(subscribe())}\n`);
		});
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			while (buffer.includes("\n")) {
				const newline = buffer.indexOf("\n");
				const line = buffer.slice(0, newline);
				buffer = buffer.slice(newline + 1);
				const frame = JSON.parse(line);
				if (frame.kind === SESSION_EVENT_KIND && frame.event.type === "session_ready") readyLine = line;
				if (frame.kind !== SESSION_REPLAY_COMPLETE_KIND) continue;
				socket.write(`${JSON.stringify({
					kind: SESSION_COMMAND_KIND,
					command_id: "command-socket",
					plot_id: "plot-01",
					session_id: "session-01",
					command: { type: "prompt", text: "through socket" },
				})}\n`);
				socket.end();
				assert.ok(readyLine);
				resolve(readyLine);
			}
		});
		socket.on("error", reject);
	});
	assert.equal(JSON.parse(firstLine).event.type, "session_ready");
	for (let attempt = 0; attempt < 100 && sent.length === 0; attempt += 1) {
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	assert.deepEqual(sent, ["through socket"]);

	await bridge.close();
	await assert.rejects(lstat(path), /ENOENT/);

	await writeFile(path, "replacement", { mode: 0o600 });
	await bridge.close();
	assert.equal(await readFile(path, "utf8"), "replacement");
});

test("Unix bridge flushes a typed fatal feed error before EOF", async (t) => {
	const root = join("/tmp", `nopal-session-fatal-flush-${process.pid}-${Date.now()}`);
	const path = join(root, "bridge.sock");
	const { engine } = harness();
	const bridge = new NopalSessionBridge({ path, engine });
	await bridge.start();
	t.after(async () => { await bridge.close(); });

	const frames = await new Promise<Array<Record<string, any>>>((resolve, reject) => {
		const socket = createConnection(path);
		const received: Array<Record<string, any>> = [];
		let buffer = "";
		socket.once("connect", () => socket.write(`${JSON.stringify(subscribe())}\n`));
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			for (;;) {
				const newline = buffer.indexOf("\n");
				if (newline < 0) break;
				const frame = JSON.parse(buffer.slice(0, newline));
				buffer = buffer.slice(newline + 1);
				received.push(frame);
				if (frame.kind === SESSION_REPLAY_COMPLETE_KIND) {
					socket.write(`${JSON.stringify({ kind: "not-a-session-command" })}\n`);
				}
			}
		});
		socket.once("end", () => resolve(received));
		socket.once("error", reject);
	});

	const fatal = frames.at(-1);
	assert.equal(fatal?.kind, SESSION_FEED_ERROR_KIND);
	assert.equal(fatal?.code, "protocol_violation");
	assert.equal(fatal?.retryable, false);
});

test("Unix bridge turns an unterminated client record into protocol_violation before EOF", async (t) => {
	const root = join("/tmp", `nopal-session-partial-eof-${process.pid}-${Date.now()}`);
	const path = join(root, "bridge.sock");
	const { engine } = harness();
	const bridge = new NopalSessionBridge({ path, engine });
	await bridge.start();
	t.after(async () => { await bridge.close(); });

	const frames = await new Promise<Array<Record<string, any>>>((resolve, reject) => {
		const socket = createConnection(path);
		const received: Array<Record<string, any>> = [];
		let buffer = "";
		socket.once("connect", () => socket.end('{"kind":"nopal.session.subscribe/v1"'));
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			for (;;) {
				const newline = buffer.indexOf("\n");
				if (newline < 0) break;
				received.push(JSON.parse(buffer.slice(0, newline)));
				buffer = buffer.slice(newline + 1);
			}
		});
		socket.once("end", () => resolve(received));
		socket.once("error", reject);
	});

	assert.equal(frames.at(-1)?.kind, SESSION_FEED_ERROR_KIND);
	assert.equal(frames.at(-1)?.code, "protocol_violation");
});

test("Unix bridge completes a large replay in order after a paused client resumes", async (t) => {
	const root = join("/tmp", `nopal-session-paused-replay-${process.pid}-${Date.now()}`);
	const path = join(root, "bridge.sock");
	const { engine } = harness();
	engine.start();
	for (let index = 0; index < 16; index += 1) {
		engine.protocolError(`large-replay-${index}-${"x".repeat(256 * 1024)}`);
	}
	const bridge = new NopalSessionBridge({ path, engine });
	await bridge.start();
	t.after(async () => { await bridge.close(); });

	const frames = await new Promise<Array<Record<string, any>>>((resolve, reject) => {
		const socket = createConnection(path);
		const received: Array<Record<string, any>> = [];
		let buffer = "";
		socket.once("connect", () => {
			socket.pause();
			socket.write(`${JSON.stringify(subscribe(null, 1))}\n`);
			setTimeout(() => socket.resume(), 50);
		});
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			for (;;) {
				const newline = buffer.indexOf("\n");
				if (newline < 0) break;
				const frame = JSON.parse(buffer.slice(0, newline));
				buffer = buffer.slice(newline + 1);
				received.push(frame);
				if (frame.kind === SESSION_REPLAY_COMPLETE_KIND) {
					socket.end();
					resolve(received);
				}
			}
		});
		socket.once("error", reject);
	});

	const events = frames.filter((frame) => frame.kind === SESSION_EVENT_KIND);
	assert.deepEqual(events.map((event) => event.sequence), Array.from({ length: 17 }, (_, index) => index + 1));
	assert.equal(frames.at(-1)?.kind, SESSION_REPLAY_COMPLETE_KIND);
	assert.equal(frames.at(-1)?.event_count, 17);
});

test("Unix bridge cancels queued live data and flushes one typed overflow error before EOF", async (t) => {
	const root = join("/tmp", `nopal-session-live-overflow-${process.pid}-${Date.now()}`);
	const path = join(root, "bridge.sock");
	const { engine } = harness();
	const bridge = new NopalSessionBridge({ path, engine });
	await bridge.start();
	t.after(async () => { await bridge.close(); });

	const frames = await new Promise<Array<Record<string, any>>>((resolve, reject) => {
		const socket = createConnection(path);
		const received: Array<Record<string, any>> = [];
		let buffer = "";
		let injected = false;
		const timeout = setTimeout(() => reject(new Error("timed out waiting for bounded overflow EOF")), 5_000);
		socket.once("connect", () => socket.write(`${JSON.stringify(subscribe())}\n`));
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			for (;;) {
				const newline = buffer.indexOf("\n");
				if (newline < 0) break;
				const frame = JSON.parse(buffer.slice(0, newline));
				buffer = buffer.slice(newline + 1);
				received.push(frame);
				if (!injected && frame.kind === SESSION_REPLAY_COMPLETE_KIND) {
					injected = true;
					socket.pause();
					for (let index = 0; index < 129; index += 1) {
						engine.protocolError(`queued-live-${index}-${"x".repeat(1024)}`);
					}
					setTimeout(() => socket.resume(), 50);
				}
			}
		});
		socket.once("end", () => {
			clearTimeout(timeout);
			resolve(received);
		});
		socket.once("error", (error) => {
			clearTimeout(timeout);
			reject(error);
		});
	});

	const errors = frames.filter((frame) => frame.kind === SESSION_FEED_ERROR_KIND);
	assert.equal(errors.length, 1);
	assert.equal(errors[0]?.code, "unavailable");
	assert.equal(errors[0]?.retryable, true);
	assert.equal(frames.at(-1)?.kind, SESSION_FEED_ERROR_KIND);
});

test("Unix bridge refuses to replace an unknown pre-existing path", async () => {
	const root = join("/tmp", `nopal-session-bridge-existing-${process.pid}-${Date.now()}`);
	await mkdir(root, { recursive: true, mode: 0o700 });
	const path = join(root, "bridge.sock");
	await writeFile(path, "not ours", { mode: 0o600 });
	const { engine } = harness();
	const bridge = new NopalSessionBridge({ path, engine });

	await assert.rejects(bridge.start(), /already exists|EADDRINUSE/);
	assert.equal(await readFile(path, "utf8"), "not ours");
});

test("endpoint descriptor advertises the unified nopal.session/v4 shape", () => {
	const { engine } = harness();
	const bridge = new NopalSessionBridge({ path: "/tmp/nopal-501/session.sock", engine });
	assert.deepEqual(bridge.endpoint(), {
		kind: SESSION_ENDPOINT_KIND,
		transport: "unix",
		address: "/tmp/nopal-501/session.sock",
		state: "ready",
	});
});

test("bridge-owned Pi activity hooks commit before broadcast and retain active command correlation", async () => {
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const order: string[] = [];
	const entries: DurableSessionEvent[] = [];
	let capturedEngine: InstanceType<typeof SessionProtocolEngine> | undefined;
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry(_customType: string, data: DurableSessionEvent) {
			order.push(`append:${data.event.type}`);
			entries.push(data);
		},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => ({ stdout: "", stderr: "", code: 0 }), {
		bridgeFactory({ path, engine }) {
			capturedEngine = engine;
			engine.subscribe((event) => order.push(`broadcast:${event.event.type}`));
			return {
				endpoint: () => ({ kind: SESSION_ENDPOINT_KIND, transport: "unix", address: path, state: "ready" }),
				async start() { engine.start(); },
				async close() { engine.close(); },
			};
		},
	});
	await registration.bind(binding);
	assert.ok(capturedEngine);
	await capturedEngine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-activity",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Run it" },
	});
	handlers.get("agent_start")?.[0]?.({ type: "agent_start" }, {});
	order.length = 0;
	const call = {
		type: "tool_call",
		toolCallId: "pi-shell-bridge",
		toolName: "bash",
		input: { command: "printf bridge" },
	};
	const result = {
		type: "tool_result",
		toolCallId: "pi-shell-bridge",
		toolName: "bash",
		input: { command: "printf bridge" },
		content: [{ type: "text", text: "bridge" }],
		details: undefined,
		isError: false,
	};
	assert.doesNotThrow(() => handlers.get("tool_call")?.[0]?.(call, {}));
	assert.doesNotThrow(() => handlers.get("tool_result")?.[0]?.(result, {}));

	assert.deepEqual(order, [
		"append:command_started",
		"broadcast:command_started",
		"append:command_finished",
		"broadcast:command_finished",
	]);
	const activity = entries.filter((event) => event.event.type.startsWith("command_"));
	assert.deepEqual(activity.map((event) => event.command_id), ["command-activity", "command-activity"]);
	assert.deepEqual(activity.map((event) => event.event.type), ["command_started", "command_finished"]);
	assert.equal(activity.some((event) => event.event.type.startsWith("tool_")), false);
	await registration.close();
});

test("activity persistence failures do not escape into Pi, broadcast, or leak correlation", async () => {
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const diagnostics: string[] = [];
	const broadcasts: string[] = [];
	const persisted: DurableSessionEvent[] = [];
	let failType: string | undefined = "command_started";
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry(_customType: string, data: DurableSessionEvent) {
			if (data.event.type === failType) throw new Error("disk API_TOKEN=top-secret");
			persisted.push(data);
		},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => ({ stdout: "", stderr: "", code: 0 }), {
		activityDiagnostic(message) { diagnostics.push(message); },
		bridgeFactory({ path, engine }) {
			engine.subscribe((event) => broadcasts.push(event.event.type));
			return {
				endpoint: () => ({ kind: SESSION_ENDPOINT_KIND, transport: "unix", address: path, state: "ready" }),
				async start() { engine.start(); },
				async close() { engine.close(); },
			};
		},
	});
	await registration.bind(binding);
	broadcasts.length = 0;
	const failedStartCall = {
		type: "tool_call",
		toolCallId: "failed-start",
		toolName: "bash",
		input: { command: "true" },
	};
	assert.doesNotThrow(() => handlers.get("tool_call")?.[0]?.(failedStartCall, {}));
	assert.deepEqual(broadcasts, []);
	assert.equal(diagnostics.length, 1);
	assert.equal(Buffer.byteLength(diagnostics[0] ?? "", "utf8") <= 4096, true);
	assert.equal(diagnostics[0]?.includes("top-secret"), false);
	assert.doesNotThrow(() => handlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "failed-start",
		toolName: "bash",
		input: { command: "true" },
		content: [],
		details: undefined,
		isError: false,
	}, {}));
	assert.equal(diagnostics.length, 2, "failed start must leave no active correlation");
	assert.deepEqual(broadcasts, []);

	failType = "command_finished";
	const failedTerminalCall = {
		type: "tool_call",
		toolCallId: "failed-terminal",
		toolName: "bash",
		input: { command: "true" },
	};
	handlers.get("tool_call")?.[0]?.(failedTerminalCall, {});
	assert.deepEqual(broadcasts, ["command_started"]);
	assert.doesNotThrow(() => handlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "failed-terminal",
		toolName: "bash",
		input: { command: "true" },
		content: [],
		details: undefined,
		isError: false,
	}, {}));
	assert.deepEqual(broadcasts, ["command_started"], "failed terminal append must not broadcast");
	assert.equal(diagnostics.length, 3);
	failType = undefined;
	assert.doesNotThrow(() => handlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "failed-terminal",
		toolName: "bash",
		input: { command: "true" },
		content: [],
		details: undefined,
		isError: false,
	}, {}));
	assert.equal(diagnostics.length, 4, "failed terminal must release in-memory correlation");
	assert.deepEqual(broadcasts, ["command_started"]);
	assert.deepEqual(
		persisted.filter((event) => event.event.type.startsWith("command_")).map((event) => event.event.type),
		["command_started"],
	);
	await registration.close();
});

test("bridge restart replays exact activity and suppresses complete duplicates without completing interrupted work", async () => {
	const firstHandlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const durable: DurableSessionEvent[] = [];
	const firstPi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			firstHandlers.set(name, [...(firstHandlers.get(name) ?? []), handler]);
		},
		appendEntry(_customType: string, data: DurableSessionEvent) { durable.push(data); },
		async sendUserMessage() {},
	};
	const bridgeFactory = ({ path, engine }: { path: string; engine: InstanceType<typeof SessionProtocolEngine> }) => ({
		endpoint: () => ({ kind: SESSION_ENDPOINT_KIND, transport: "unix" as const, address: path, state: "ready" }),
		async start() { engine.start(); },
		async close() { engine.close(); },
	});
	const first = registerNopalSessionBridge(firstPi as any, async () => ({ stdout: "", stderr: "", code: 0 }), {
		bridgeFactory,
	});
	await first.bind(binding);
	const completeCall = {
		type: "tool_call",
		toolCallId: "bridge-restart-complete",
		toolName: "read",
		input: { path: "/repo/one" },
	};
	firstHandlers.get("tool_call")?.[0]?.(completeCall, {});
	firstHandlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "bridge-restart-complete",
		toolName: "read",
		input: { path: "/repo/one" },
		content: [{ type: "text", text: "contents" }],
		details: undefined,
		isError: false,
	}, {});
	firstHandlers.get("tool_call")?.[0]?.({
		type: "tool_call",
		toolCallId: "bridge-restart-interrupted",
		toolName: "grep",
		input: { pattern: "needle", path: "/repo" },
	}, {});
	await first.close();
	const exactPrefix = structuredClone(durable);
	const branch = durableBranch(exactPrefix);

	const secondHandlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const restartedAppends: DurableSessionEvent[] = [];
	const diagnostics: string[] = [];
	const secondPi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			secondHandlers.set(name, [...(secondHandlers.get(name) ?? []), handler]);
		},
		appendEntry(_customType: string, data: DurableSessionEvent) { restartedAppends.push(data); },
		async sendUserMessage() {},
	};
	const restarted = registerNopalSessionBridge(secondPi as any, async () => ({ stdout: "", stderr: "", code: 0 }), {
		bridgeFactory,
		history: { getBranch: () => branch },
		activityDiagnostic(message) { diagnostics.push(message); },
	});
	await restarted.bind(binding);
	secondHandlers.get("tool_call")?.[0]?.(structuredClone(completeCall), {});
	secondHandlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "bridge-restart-complete",
		toolName: "read",
		input: { path: "/repo/one" },
		content: [{ type: "text", text: "contents" }],
		details: undefined,
		isError: false,
	}, {});
	assert.deepEqual(restartedAppends, [], "complete replay must suppress stable duplicate identities and cursors");
	secondHandlers.get("tool_result")?.[0]?.({
		type: "tool_result",
		toolCallId: "bridge-restart-interrupted",
		toolName: "grep",
		input: { pattern: "needle", path: "/repo" },
		content: [],
		details: undefined,
		isError: false,
	}, {});
	assert.deepEqual(restartedAppends, []);
	assert.equal(diagnostics.length, 1);
	assert.match(diagnostics[0] ?? "", /without a local monotonic start/u);
	assert.deepEqual(exactPrefix, durable);
	await restarted.close();
});

test("registration can refresh after fresh-Session identity appears without a Pi restart", async (t) => {
	const root = join("/tmp", `nopal-session-refresh-${process.pid}-${Date.now()}`);
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const entries: unknown[] = [];
	let identityReady = false;
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry(_customType: string, data: unknown) { entries.push(data); },
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async (_command, args) => ({
		stdout: identityReady ? (args.at(-1) === "@nopal_plot" ? "plot-01\n" : "session-01\n") : "",
		stderr: "",
		code: 0,
	}), { runtimeRoot: root, paneId: "%7" });
	t.after(async () => { await registration.close(); });

	await handlers.get("session_start")?.[0]?.({ type: "session_start", reason: "startup" }, { cwd: "/repo" });
	assert.equal(registration.endpoint(), undefined);

	identityReady = true;
	const endpoint = await registration.refresh("/repo");
	assert.equal(endpoint?.address.startsWith(root), true);
	assert.equal(endpoint?.state, "ready");
	assert.equal(entries.length, 1);
});

test("registration defers the second rapid prompt until the first outer loop ends", async (t) => {
	const root = join("/tmp", `nopal-session-fifo-${process.pid}-${Date.now()}`);
	const deliveries: Array<{ text: string; options: unknown }> = [];
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry() {},
		async sendUserMessage(text: string, options?: unknown) {
			deliveries.push({ text, options });
		},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => ({
		stdout: "",
		stderr: "",
		code: 0,
	}), { runtimeRoot: root });
	t.after(async () => { await registration.close(); });
	const endpoint = await registration.bind(binding);
	assert.ok(endpoint);

	await new Promise<void>((resolve, reject) => {
		const socket = createConnection(endpoint.address);
		let buffer = "";
		let commandsSent = false;
		socket.once("connect", () => {
			socket.write(`${JSON.stringify(subscribe())}\n`);
		});
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			while (buffer.includes("\n")) {
				const newline = buffer.indexOf("\n");
				const frame = JSON.parse(buffer.slice(0, newline));
				buffer = buffer.slice(newline + 1);
				if (frame.kind !== SESSION_REPLAY_COMPLETE_KIND || commandsSent) continue;
				commandsSent = true;
				for (const [commandId, text] of [["command-01", "first"], ["command-02", "second"]]) {
					socket.write(`${JSON.stringify({
						kind: SESSION_COMMAND_KIND,
						command_id: commandId,
						plot_id: binding.plotId,
						session_id: binding.sessionId,
						command: { type: "prompt", text },
					})}\n`);
				}
				socket.end(resolve);
			}
		});
		socket.on("error", reject);
	});
	for (let attempt = 0; attempt < 100 && deliveries.length < 1; attempt += 1) {
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	await new Promise((resolve) => setTimeout(resolve, 20));
	assert.deepEqual(deliveries, [{ text: "first", options: undefined }]);
	handlers.get("agent_start")?.[0]?.({ type: "agent_start" }, {});
	handlers.get("agent_end")?.[0]?.({ type: "agent_end" }, {});
	for (let attempt = 0; attempt < 100 && deliveries.length < 2; attempt += 1) {
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	assert.deepEqual(deliveries, [
		{ text: "first", options: undefined },
		{ text: "second", options: undefined },
	]);
});

test("overlapping binds serialize by requested identity and remove the old socket", async (t) => {
	const root = join("/tmp", `nopal-session-overlap-${process.pid}-${Date.now()}`);
	const pi = {
		on() {},
		appendEntry() {},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => ({
		stdout: "",
		stderr: "",
		code: 0,
	}), { runtimeRoot: root });
	t.after(async () => { await registration.close(); });
	const oldBinding = { plotId: "plot-old", sessionId: "session-old" };
	const newBinding = { plotId: "plot-new", sessionId: "session-new" };

	const oldPromise = registration.bind(oldBinding);
	const newPromise = registration.bind(newBinding);
	const [oldEndpoint, newEndpoint] = await Promise.all([oldPromise, newPromise]);

	assert.equal(oldEndpoint?.address, defaultSessionSocketPath(oldBinding, root));
	assert.equal(newEndpoint?.address, defaultSessionSocketPath(newBinding, root));
	assert.equal(registration.endpoint()?.address, newEndpoint?.address);
	await assert.rejects(lstat(defaultSessionSocketPath(oldBinding, root)), /ENOENT/);
	assert.equal((await lstat(defaultSessionSocketPath(newBinding, root))).mode & 0o777, 0o600);
});

test("a delayed refresh cannot overwrite a later explicitly requested binding", async (t) => {
	const root = join("/tmp", `nopal-session-refresh-order-${process.pid}-${Date.now()}`);
	let releaseOldResolution!: () => void;
	const oldResolution = new Promise<void>((resolve) => { releaseOldResolution = resolve; });
	let markResolutionStarted!: () => void;
	const resolutionStarted = new Promise<void>((resolve) => { markResolutionStarted = resolve; });
	const oldBinding = { plotId: "plot-refresh-old", sessionId: "session-refresh-old" };
	const newBinding = { plotId: "plot-explicit-new", sessionId: "session-explicit-new" };
	const pi = {
		on() {},
		appendEntry() {},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async (_command, args) => {
		markResolutionStarted();
		await oldResolution;
		return {
			stdout: `${args.at(-1) === "@nopal_plot" ? oldBinding.plotId : oldBinding.sessionId}\n`,
			stderr: "",
			code: 0,
		};
	}, { runtimeRoot: root, paneId: "%7" });
	t.after(async () => { await registration.close(); });

	const refreshPromise = registration.refresh("/old-worktree");
	await resolutionStarted;
	const newPromise = registration.bind(newBinding);
	releaseOldResolution();
	const [oldEndpoint, newEndpoint] = await Promise.all([refreshPromise, newPromise]);

	assert.equal(oldEndpoint?.address, defaultSessionSocketPath(oldBinding, root));
	assert.equal(newEndpoint?.address, defaultSessionSocketPath(newBinding, root));
	assert.equal(registration.endpoint()?.address, newEndpoint?.address);
	await assert.rejects(lstat(defaultSessionSocketPath(oldBinding, root)), /ENOENT/);
	assert.equal((await lstat(defaultSessionSocketPath(newBinding, root))).mode & 0o777, 0o600);
});

test("shutdown invalidates queued Session start work and later binds cannot reopen it", async (t) => {
	const root = join("/tmp", `nopal-session-shutdown-order-${process.pid}-${Date.now()}`);
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	let releaseResolution!: () => void;
	const heldResolution = new Promise<void>((resolve) => { releaseResolution = resolve; });
	let markResolutionStarted!: () => void;
	const resolutionStarted = new Promise<void>((resolve) => { markResolutionStarted = resolve; });
	const oldBinding = { plotId: "plot-shutdown-old", sessionId: "session-shutdown-old" };
	const lateBinding = { plotId: "plot-shutdown-late", sessionId: "session-shutdown-late" };
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry() {},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async (_command, args) => {
		markResolutionStarted();
		await heldResolution;
		return {
			stdout: `${args.at(-1) === "@nopal_plot" ? oldBinding.plotId : oldBinding.sessionId}\n`,
			stderr: "",
			code: 0,
		};
	}, { runtimeRoot: root, paneId: "%7" });
	t.after(async () => { await registration.close(); });

	const refreshPromise = registration.refresh("/old-worktree");
	await resolutionStarted;
	const startPromise = handlers.get("session_start")?.[0]?.(
		{ type: "session_start", reason: "switch" },
		{ cwd: "/queued-start" },
	);
	const shutdownPromise = handlers.get("session_shutdown")?.[0]?.(
		{ type: "session_shutdown", reason: "quit" },
		{},
	);
	const lateBindPromise = registration.bind(lateBinding);
	releaseResolution();
	const [refreshEndpoint, startEndpoint, , lateEndpoint] = await Promise.all([
		refreshPromise,
		startPromise,
		shutdownPromise,
		lateBindPromise,
	]);

	assert.equal(refreshEndpoint, undefined);
	assert.equal(startEndpoint, undefined);
	assert.equal(lateEndpoint, undefined);
	assert.equal(registration.endpoint(), undefined);
	await assert.rejects(lstat(defaultSessionSocketPath(oldBinding, root)), /ENOENT/);
	await assert.rejects(lstat(defaultSessionSocketPath(lateBinding, root)), /ENOENT/);
});

test("a genuinely later Session start can reopen after shutdown", async (t) => {
	const root = join("/tmp", `nopal-session-restart-${process.pid}-${Date.now()}`);
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const restartedBinding = { plotId: "plot-restarted", sessionId: "session-restarted" };
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry() {},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async (_command, args) => ({
		stdout: `${args.at(-1) === "@nopal_plot" ? restartedBinding.plotId : restartedBinding.sessionId}\n`,
		stderr: "",
		code: 0,
	}), { runtimeRoot: root, paneId: "%7" });
	t.after(async () => { await registration.close(); });

	await handlers.get("session_shutdown")?.[0]?.({ type: "session_shutdown" }, {});
	assert.equal(registration.endpoint(), undefined);
	await handlers.get("session_start")?.[0]?.({ type: "session_start" }, { cwd: "/restarted" });

	assert.equal(
		registration.endpoint()?.address,
		defaultSessionSocketPath(restartedBinding, root),
	);
});

test("explicit registration close permanently prevents bind refresh and Session restart", async () => {
	const root = join("/tmp", `nopal-session-permanent-close-${process.pid}-${Date.now()}`);
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	let resolutionCalls = 0;
	const binding = { plotId: "plot-closed", sessionId: "session-closed" };
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry() {},
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => {
		resolutionCalls += 1;
		return { stdout: "unreachable\n", stderr: "", code: 0 };
	}, { runtimeRoot: root, paneId: "%7" });

	await registration.close();
	const [bindEndpoint, refreshEndpoint] = await Promise.all([
		registration.bind(binding),
		registration.refresh("/closed"),
		handlers.get("session_start")?.[0]?.({ type: "session_start" }, { cwd: "/closed" }),
	]);

	assert.equal(bindEndpoint, undefined);
	assert.equal(refreshEndpoint, undefined);
	assert.equal(registration.endpoint(), undefined);
	assert.equal(resolutionCalls, 0);
	await assert.rejects(lstat(defaultSessionSocketPath(binding, root)), /ENOENT/);
});

test("durable restart reuses ready and records one interrupted command without redelivery", async () => {
	const first = harness();
	const ready = first.engine.start();
	await first.engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-interrupted",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Do not redeliver" },
	});
	const firstEvents = first.entries.map((entry) => entry.data as DurableSessionEvent);
	assert.equal(ready.kind, SESSION_EVENT_KIND);
	assert.equal(ready.sequence, 1);

	const restartedEntries: DurableSessionEvent[] = [];
	const restarted = new SessionProtocolEngine(binding, {
		activeBranch: durableBranch(firstEvents),
		appendEntry(_type, data) { restartedEntries.push(data); },
		async sendUserMessage() { assert.fail("persisted command must not be redelivered"); },
		nextId: () => crypto.randomUUID(),
	});
	assert.equal(restarted.start().cursor, ready.cursor);
	assert.equal(restartedEntries.length, 1);
	assert.equal(restartedEntries[0]?.command_id, "command-interrupted");
	assert.equal(restartedEntries[0]?.event.type, "session_error");

	const secondRestartEntries: DurableSessionEvent[] = [];
	const secondRestart = new SessionProtocolEngine(binding, {
		activeBranch: durableBranch([...firstEvents, ...restartedEntries]),
		appendEntry(_type, data) { secondRestartEntries.push(data); },
		async sendUserMessage() { assert.fail("completed interruption must not redeliver"); },
	});
	secondRestart.start();
	assert.deepEqual(secondRestartEntries, []);
});

test("cold replay and cursor resume end at one exact snapshot completion", async () => {
	const { engine } = harness();
	const ready = engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Question" },
	});
	engine.observeAgentStart();
	engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Answer" }] });
	engine.observeAgentEnd();
	const snapshot = engine.log.events();

	const cold = feedHarness(engine);
	await cold.feed.accept(subscribe(null, 2));
	assert.deepEqual(cold.frames.slice(0, -1), snapshot);
	assert.deepEqual(cold.frames.at(-1), {
		kind: SESSION_REPLAY_COMPLETE_KIND,
		request_id: "request-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		stream_id: engine.log.streamId,
		cursor: engine.log.headCursor,
		sequence: engine.log.headSequence,
		event_count: snapshot.length,
	});

	const resumed = feedHarness(engine);
	await resumed.feed.accept(subscribe(ready.cursor, 1));
	assert.deepEqual(resumed.frames.slice(0, -1), snapshot.slice(1));
	assert.equal(resumed.frames.at(-1)?.event_count, snapshot.length - 1);
});

test("v4 replay publishes Pi model state and one exact switch acknowledgement", async () => {
	const { engine } = harness();
	engine.start();
	const models = [
		{ provider: "nopal-proof", id: "deterministic-a", name: "Model A" },
		{ provider: "nopal-proof", id: "deterministic-b", name: "Model B" },
	];
	let current = models[0];
	const controller = new SessionModelController(binding, {
		available: () => models,
		current: () => current,
		isIdle: () => true,
		async setModel(model) {
			current = model;
			return true;
		},
	});
	const state = feedHarness(engine, undefined, undefined, controller);
	await state.feed.accept(subscribe());
	assert.equal(state.frames.at(-2)?.kind, SESSION_REPLAY_COMPLETE_KIND);
	assert.equal(state.frames.at(-1)?.kind, SESSION_MODEL_STATE_KIND);
	assert.equal(state.frames.at(-1)?.current.id, "deterministic-a");

	await state.feed.accept({
		kind: SESSION_MODEL_REQUEST_KIND,
		request_id: "switch-over-v4",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		request: {
			type: "switch",
			model: { provider: "nopal-proof", id: "deterministic-b" },
		},
	});
	assert.equal(state.frames.at(-1)?.kind, SESSION_MODEL_STATE_KIND);
	assert.equal(state.frames.at(-1)?.request_id, "switch-over-v4");
	assert.equal(state.frames.at(-1)?.current.id, "deterministic-b");
});

test("subscribe snapshots replay, buffers later live events, and drains after completion", async () => {
	const { engine } = harness();
	engine.start();
	await engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Question" },
	});
	const snapshot = [...engine.log.events()];
	let injected = false;
	const state = feedHarness(engine, () => {
		if (injected) return;
		injected = true;
		engine.protocolError("late lifecycle event");
	});
	await state.feed.accept(subscribe(null, 1));
	assert.deepEqual(state.frames.slice(0, snapshot.length), snapshot);
	assert.equal(state.frames[snapshot.length]?.kind, SESSION_REPLAY_COMPLETE_KIND);
	assert.equal(state.frames.at(-1)?.kind, SESSION_EVENT_KIND);
});

test("live events published during an asynchronous replay drain cannot overtake buffered events", async () => {
	const { engine } = harness();
	engine.start();
	let injected = false;
	let markDrainStarted!: () => void;
	const drainStarted = new Promise<void>((resolve) => { markDrainStarted = resolve; });
	let releaseDrain!: () => void;
	const drainRelease = new Promise<void>((resolve) => { releaseDrain = resolve; });
	const state = feedHarness(
		engine,
		() => {
			if (injected) return;
			injected = true;
			engine.protocolError("buffered before drain");
		},
		async (frame) => {
			if (frame.kind === SESSION_EVENT_KIND && frame.event.message === "buffered before drain") {
				markDrainStarted();
				await drainRelease;
			}
			state.frames.push(frame);
		},
	);

	const replay = state.feed.accept(subscribe(null, 1));
	await drainStarted;
	engine.protocolError("published during drain");
	releaseDrain();
	await replay;

	assert.deepEqual(
		state.frames
			.filter((frame) => frame.kind === SESSION_EVENT_KIND && frame.event.type === "session_error")
			.map((frame) => frame.event.message),
		["buffered before drain", "published during drain"],
	);
});

test("activity published during replay remains after the exact snapshot and replay boundary", async () => {
	const { engine } = harness();
	engine.start();
	const summary = {
		text: "Read /repo/one",
		truncated: false,
		original_bytes: 14,
		omitted_bytes: 0,
	};
	engine.publishActivity({
		eventId: "activity-start-one",
		event: {
			type: "tool_started",
			activity_id: "activity-one",
			tool_call_id: "tool-one",
			tool_name: "read",
			summary,
			started_at: "2026-07-13T10:00:00.000Z",
		},
	});
	const snapshot = engine.log.events();
	let injected = false;
	const state = feedHarness(engine, () => {
		if (injected) return;
		injected = true;
		engine.publishActivity({
			eventId: "activity-start-two",
			event: {
				type: "tool_started",
				activity_id: "activity-two",
				tool_call_id: "tool-two",
				tool_name: "read",
				summary,
				started_at: "2026-07-13T10:00:01.000Z",
			},
		});
	});
	await state.feed.accept(subscribe(null, 1));

	assert.deepEqual(state.frames.slice(0, snapshot.length), snapshot);
	assert.equal(state.frames[snapshot.length]?.kind, SESSION_REPLAY_COMPLETE_KIND);
	assert.equal(state.frames.at(-1)?.event.tool_call_id, "tool-two");
});

test("a replay writer failure closes with one retryable unavailable feed error", async () => {
	const { engine } = harness();
	engine.start();
	const written: Array<Record<string, any>> = [];
	let rejectReplay = true;
	const state = feedHarness(engine, undefined, (frame) => {
		if (rejectReplay && frame.kind === SESSION_EVENT_KIND) {
			rejectReplay = false;
			throw new Error("output queue timed out");
		}
		written.push(frame);
	});

	await state.feed.accept(subscribe());

	assert.equal(state.closeCount(), 1);
	assert.deepEqual(written.map((frame) => ({
		kind: frame.kind,
		code: frame.code,
		retryable: frame.retryable,
	})), [{
		kind: SESSION_FEED_ERROR_KIND,
		code: "unavailable",
		retryable: true,
	}]);
});

test("client must subscribe before commands and conflicting duplicates are targeted feed errors", async () => {
	const { engine } = harness();
	engine.start();
	const unordered = feedHarness(engine);
	await unordered.feed.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-early",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "too early" },
	});
	assert.equal(unordered.frames[0]?.code, "protocol_violation");
	assert.equal(unordered.closeCount(), 1);
	const foreign = feedHarness(engine);
	await foreign.feed.accept({ ...subscribe(), request_id: "foreign-request", session_id: "session-other" });
	assert.equal(foreign.frames[0]?.code, "foreign_session");
	assert.equal(foreign.frames[0]?.request_id, "foreign-request");

	const subscribed = feedHarness(engine);
	await subscribed.feed.accept(subscribe());
	await subscribed.feed.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Same" },
	});
	const eventCount = engine.log.eventCount;
	await subscribed.feed.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Same" },
	});
	assert.equal(engine.log.eventCount, eventCount, "exact retry is a no-op");
	await subscribed.feed.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Different" },
	});
	assert.equal(subscribed.frames.at(-1)?.kind, SESSION_FEED_ERROR_KIND);
	assert.equal(subscribed.frames.at(-1)?.code, "command_conflict");
});

test("known abandoned suffix differs from a fabricated same-stream history gap", async () => {
	const original = harness();
	original.engine.start();
	await original.engine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-01",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Question" },
	});
	original.engine.observeAgentStart();
	original.engine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Old answer" }] });
	original.engine.observeAgentEnd();
	const [ready, user, abandoned] = original.engine.log.events();
	assert.ok(ready && user && abandoned);

	const branched = new SessionProtocolEngine(binding, {
		activeBranch: durableBranch([ready, user]),
		abandonedCursors: [abandoned.cursor],
		appendEntry() {},
		async sendUserMessage() {},
	});
	branched.start();
	const known = feedHarness(branched);
	await known.feed.accept(subscribe(abandoned.cursor));
	assert.equal(known.frames[0]?.code, "branch_diverged");

	const fabricated = abandoned.cursor.replace(/[0-9a-f]{64}$/u, "a".repeat(64));
	const unknown = feedHarness(branched);
	await unknown.feed.accept(subscribe(fabricated));
	assert.equal(unknown.frames[0]?.code, "history_gap");
});

test("session_tree rehydrates only getBranch and registers the old active suffix as abandoned", async () => {
	const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
	const persisted: DurableSessionEvent[] = [];
	const engines: Array<InstanceType<typeof SessionProtocolEngine>> = [];
	let branch: PiSessionEntry[] = [];
	const history = { getBranch: () => branch };
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => unknown) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		appendEntry(_type: string, data: DurableSessionEvent) { persisted.push(data); },
		async sendUserMessage() {},
	};
	const registration = registerNopalSessionBridge(pi as any, async () => ({ stdout: "", stderr: "", code: 0 }), {
		history,
		bridgeFactory({ engine }) {
			engines.push(engine);
			return {
				endpoint: () => ({ kind: SESSION_ENDPOINT_KIND, transport: "unix", address: "/virtual/session.sock", state: "ready" }),
				async start() { engine.start(); },
				async close() { engine.close(); },
			};
		},
	});
	await registration.bind(binding, history);
	const firstEngine = engines.at(-1);
	assert.ok(firstEngine);
	await firstEngine.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-branched",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Pi structured turn" },
	});
	firstEngine.observeAgentStart();
	firstEngine.observeAssistant({ role: "assistant", content: [{ type: "text", text: "Old answer" }] });
	firstEngine.observeAgentEnd();
	const [ready, user, abandoned] = firstEngine.log.events();
	assert.ok(ready && user && abandoned);

	branch = durableBranch([ready, user]);
	await handlers.get("session_tree")?.[0]?.(
		{ type: "session_tree", oldLeafId: "old", newLeafId: "new" },
		{ sessionManager: history },
	);
	const branchedEngine = engines.at(-1);
	assert.ok(branchedEngine && branchedEngine !== firstEngine);
	const feed = feedHarness(branchedEngine);
	await feed.feed.accept(subscribe(abandoned.cursor));
	assert.equal(feed.frames[0]?.code, "branch_diverged");
	assert.equal(
		branchedEngine.log.events().filter((event) => event.event.type === "session_error").length,
		1,
		"tree rehydrate records one interruption for the now-unanswered active user command",
	);
	await registration.close();
});

test("corrupt persisted history exposes one bounded typed fault over the ready Unix endpoint", async () => {
	const valid = harness();
	const ready = valid.engine.start();
	const cases = [
		{
			name: "malformed",
			code: "history_corrupt",
			data: {
				kind: "nopal.session.event/v1",
				event_id: "event-malformed",
				plot_id: binding.plotId,
				session_id: binding.sessionId,
				event: { type: "unknown" },
			},
		},
		{
			name: "foreign",
			code: "foreign_session",
			data: {
				kind: "nopal.session.event/v1",
				event_id: "event-foreign",
				plot_id: "plot-foreign",
				session_id: binding.sessionId,
				event: { type: "session_error", message: "foreign" },
			},
		},
		{
			name: "oversized",
			code: "history_too_large",
			data: {
				kind: "nopal.session.event/v1",
				event_id: "event-oversized",
				plot_id: binding.plotId,
				session_id: binding.sessionId,
				event: { type: "session_error", message: "x".repeat(MAX_JSONL_LINE_BYTES) },
			},
		},
	] as const;

	for (const fault of cases) {
		const root = join("/tmp", `nopal-session-history-${fault.name}-${process.pid}-${Date.now()}`);
		let factoryCalls = 0;
		const branch: PiSessionEntry[] = [
			...durableBranch([ready]),
			{
				type: "custom",
				id: `durable-${fault.name}`,
				parentId: "durable-1",
				customType: SESSION_EVENT_ENTRY,
				data: fault.data,
			},
		];
		const pi = {
			on() {},
			appendEntry() { assert.fail("fault endpoint must not append beyond the verified prefix"); },
			async sendUserMessage() {},
		};
		const registration = registerNopalSessionBridge(
			pi as any,
			async () => ({ stdout: "", stderr: "", code: 0 }),
			{
				runtimeRoot: root,
				history: { getBranch: () => branch },
				bridgeFactory() {
					factoryCalls += 1;
					assert.fail("a corrupt history has no valid engine for a custom bridge factory");
				},
			},
		);
		try {
			const endpoint = await registration.bind(binding);
			assert.ok(endpoint);
			assert.equal(endpoint.state, "ready");
			assert.equal(registration.endpoint()?.address, endpoint.address);
			assert.equal((await lstat(endpoint.address)).mode & 0o777, 0o600);
			assert.equal(factoryCalls, 0);

			const frames = await new Promise<Array<Record<string, any>>>((resolve, reject) => {
				const socket = createConnection(endpoint.address);
				const received: Array<Record<string, any>> = [];
				let buffer = "";
				const timeout = setTimeout(() => reject(new Error("timed out waiting for persisted-history fault EOF")), 5_000);
				socket.on("data", (chunk) => {
					buffer += chunk.toString("utf8");
					for (;;) {
						const newline = buffer.indexOf("\n");
						if (newline < 0) break;
						received.push(JSON.parse(buffer.slice(0, newline)));
						buffer = buffer.slice(newline + 1);
					}
				});
				socket.once("end", () => {
					clearTimeout(timeout);
					resolve(received);
				});
				socket.once("error", (error) => {
					clearTimeout(timeout);
					reject(error);
				});
			});

			assert.equal(frames.length, 1);
			assert.deepEqual({
				kind: frames[0]?.kind,
				request_id: frames[0]?.request_id,
				plot_id: frames[0]?.plot_id,
				session_id: frames[0]?.session_id,
				code: frames[0]?.code,
				retryable: frames[0]?.retryable,
			}, {
				kind: SESSION_FEED_ERROR_KIND,
				request_id: null,
				plot_id: binding.plotId,
				session_id: binding.sessionId,
				code: fault.code,
				retryable: false,
			});
			assert.ok(Buffer.byteLength(frames[0]?.message ?? "", "utf8") <= 4096);
		} finally {
			await registration.close();
		}
		await assert.rejects(lstat(defaultSessionSocketPath(binding, root)), /ENOENT/);
	}
});

test("replay live-buffer overflow is visible and persistence failure never reaches Pi", async () => {
	const { engine } = harness();
	engine.start();
	let injected = false;
	const overflowing = feedHarness(engine, () => {
		if (injected) return;
		injected = true;
		for (let index = 0; index < 129; index += 1) engine.protocolError(`late-${index}`);
	});
	await overflowing.feed.accept(subscribe(null, 1));
	const eventOverflow = overflowing.frames.find((frame) => frame.code === "replay_buffer_overflow");
	assert.equal(eventOverflow?.kind, SESSION_FEED_ERROR_KIND);
	assert.equal(eventOverflow?.retryable, true);
	assert.equal(overflowing.closeCount(), 1);
	assert.equal(overflowing.frames.some((frame) => frame.kind === SESSION_REPLAY_COMPLETE_KIND), false);

	const byteBounded = harness();
	byteBounded.engine.start();
	let injectedBytes = false;
	const byteOverflow = feedHarness(byteBounded.engine, () => {
		if (injectedBytes) return;
		injectedBytes = true;
		for (let index = 0; index < 10; index += 1) {
			byteBounded.engine.protocolError(`large-${index}-${"x".repeat(900_000)}`);
		}
	});
	await byteOverflow.feed.accept(subscribe(null, 1));
	const byteOverflowError = byteOverflow.frames.find((frame) => frame.code === "replay_buffer_overflow");
	assert.equal(byteOverflowError?.kind, SESSION_FEED_ERROR_KIND);
	assert.equal(byteOverflowError?.retryable, true);
	assert.equal(byteOverflow.closeCount(), 1);

	let failPersistence = false;
	const delivered: string[] = [];
	const failing = new SessionProtocolEngine(binding, {
		activeBranch: [],
		appendEntry() { if (failPersistence) throw new Error("disk full"); },
		async sendUserMessage(text) { delivered.push(text); },
	});
	failing.start();
	const client = feedHarness(failing);
	await client.feed.accept(subscribe());
	client.frames.length = 0;
	failPersistence = true;
	await client.feed.accept({
		kind: SESSION_COMMAND_KIND,
		command_id: "command-fail",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command: { type: "prompt", text: "Never deliver" },
	});
	assert.deepEqual(delivered, []);
	assert.equal(client.frames[0]?.code, "internal");
});
