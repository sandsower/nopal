import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import {
	DEFAULT_MAX_DURABLE_SESSION_BYTES,
	DEFAULT_MAX_DURABLE_SESSION_EVENT_BYTES,
	DEFAULT_MAX_DURABLE_SESSION_EVENTS,
	DEFAULT_MAX_REPLAY_PAGE_EVENTS,
	DurableSessionLog,
	DurableSessionLogError,
	SESSION_EVENT_ENTRY,
	SESSION_EVENT_V1_KIND,
	SESSION_EVENT_V2_KIND,
	type DurableSessionEvent,
	type PiSessionEntry,
} from "../session-log.ts";

const binding = { plotId: "plot-01", sessionId: "session-01" };
const mixedBinding = { plotId: "plot-fixture", sessionId: "session-fixture" };

function mixedReplayEvents(): DurableSessionEvent[] {
	const fixture = readFileSync(
		new URL("../../../conformance/surface/session/mixed-replay-v3.jsonl", import.meta.url),
		"utf8",
	).trim().split("\n").map((line) => JSON.parse(line) as Record<string, unknown>);
	return fixture.slice(1, -1) as DurableSessionEvent[];
}

function legacyEvent(
	eventId: string,
	event: Record<string, unknown>,
	commandId?: string,
): Record<string, unknown> {
	return {
		kind: SESSION_EVENT_V1_KIND,
		event_id: eventId,
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		...(commandId === undefined ? {} : { command_id: commandId }),
		event,
	};
}

function customEntry(id: string, data: unknown): PiSessionEntry {
	return {
		type: "custom",
		id,
		parentId: null,
		customType: SESSION_EVENT_ENTRY,
		data,
	};
}

function hydrate(
	activeBranch: readonly PiSessionEntry[],
	appendEntry: (customType: string, data: unknown) => void = () => undefined,
	limits?: { maxEvents?: number; maxBytes?: number; maxEventBytes?: number; maxReplayPageEvents?: number },
	abandonedCursors?: readonly string[],
): DurableSessionLog {
	return DurableSessionLog.hydrate({ binding, activeBranch, appendEntry, limits, abandonedCursors });
}

function expectCode(action: () => unknown, code: string): DurableSessionLogError {
	let caught: unknown;
	try {
		action();
	} catch (error) {
		caught = error;
	}
	assert.ok(caught instanceof DurableSessionLogError);
	assert.equal(caught.code, code);
	return caught;
}

test("hydrates the active Pi branch and migrates legacy events deterministically", () => {
	const activeBranch = [
		{ type: "message", id: "pi-user", parentId: null },
		customEntry("pi-ready", legacyEvent("event-ready", { type: "session_ready", future: true })),
		customEntry("pi-user-event", legacyEvent("event-user", { type: "user_message", text: "Inspect" }, "command-01")),
		customEntry("pi-assistant", legacyEvent("event-assistant", { type: "assistant_message", text: "Done" }, "command-01")),
	] satisfies PiSessionEntry[];

	const first = hydrate(activeBranch);
	const second = hydrate(activeBranch);
	const events = first.events();

	assert.equal(first.streamId, second.streamId);
	assert.match(first.streamId, /^nopal\.session\.stream\/v1:[0-9a-f]{64}$/u);
	assert.deepEqual(events, second.events());
	assert.deepEqual(events.map((event) => event.event_id), ["event-ready", "event-user", "event-assistant"]);
	assert.deepEqual(events.map((event) => event.kind), [SESSION_EVENT_V2_KIND, SESSION_EVENT_V2_KIND, SESSION_EVENT_V2_KIND]);
	assert.deepEqual(events.map((event) => event.sequence), [1, 2, 3]);
	assert.equal(events[0]?.previous_cursor, null);
	assert.equal(events[1]?.previous_cursor, events[0]?.cursor);
	assert.equal(events[2]?.previous_cursor, events[1]?.cursor);
	assert.equal(first.headCursor, events[2]?.cursor);
	assert.equal(first.ready()?.event_id, "event-ready");
	assert.equal(first.classifyCommand("command-01", "Inspect").kind, "duplicate");
	expectCode(() => first.classifyCommand("command-01", "Different"), "command_conflict");
});

test("rehydrates committed v2 entries exactly and preserves canonical payload cursors", () => {
	const persisted: DurableSessionEvent[] = [];
	const writer = hydrate([], (_customType, data) => persisted.push(data as DurableSessionEvent));
	const first = writer.append({
		eventId: "event-ready",
		event: { future_z: [2, 1], type: "session_ready", future_a: { z: true, a: null } },
	});
	const second = writer.append({
		eventId: "event-user",
		commandId: "command-01",
		event: { text: "Inspect", type: "user_message" },
	});

	const reader = hydrate(persisted.map((event, index) => customEntry(`pi-${index}`, event)));
	assert.deepEqual(reader.events(), [first, second]);
	assert.equal(reader.headCursor, writer.headCursor);
	assert.equal(reader.byteCount, writer.byteCount);
	assert.equal(reader.classifyCommand("command-01", "Inspect").kind, "duplicate");
	assert.equal(reader.headSequence, 2);
});

test("preserves an exact v2 prefix while a v3 log resumes and appends on the same chain", () => {
	const fixture = mixedReplayEvents();
	const exactV2Prefix = structuredClone(fixture.slice(0, 2));
	const [ready, user] = exactV2Prefix;
	assert.ok(ready && user);
	const persisted = structuredClone(exactV2Prefix);

	const upgraded = DurableSessionLog.hydrate({
		binding: mixedBinding,
		activeBranch: persisted.map((event, index) => customEntry(`v2-${index}`, event)),
		appendEntry(_customType, data) {
			persisted.push(data);
		},
		appendKind: "nopal.session.event/v3",
	});
	assert.deepEqual(upgraded.events(), exactV2Prefix);
	assert.deepEqual(upgraded.eventsAfter(user.cursor).events, []);

	const started = upgraded.append({
		eventId: "event-command-started",
		commandId: "command-fixture",
		event: {
			activity_id: "activity-shell-01",
			command: "cargo test -p nopal-feed-client",
			started_at: "2026-07-13T11:00:00Z",
			tool_call_id: "tool-call-shell-01",
			type: "command_started",
			working_directory: "workspace",
		},
		extra: {
			future_activity_fact: { source: "pi-hook" },
		},
	});
	const finished = upgraded.append({
		eventId: "event-command-finished",
		commandId: "command-fixture",
		event: {
			activity_id: "activity-shell-01",
			duration_ms: 418,
			exit: { code: 0, type: "code" },
			outcome: "succeeded",
			output: {
				channel: "combined",
				omitted_bytes: 0,
				original_bytes: 15,
				text: "test result: ok",
				truncated: false,
			},
			tool_call_id: "tool-call-shell-01",
			type: "command_finished",
		},
	});

	assert.deepEqual(upgraded.events().slice(0, 2), exactV2Prefix);
	assert.deepEqual(persisted.slice(0, 2), exactV2Prefix);
	assert.equal(started.kind, "nopal.session.event/v3");
	assert.equal(started.stream_id, ready.stream_id);
	assert.equal(started.sequence, 3);
	assert.equal(started.previous_cursor, user.cursor);
	assert.equal(finished.kind, "nopal.session.event/v3");
	assert.equal(finished.stream_id, ready.stream_id);
	assert.equal(finished.sequence, 4);
	assert.equal(finished.previous_cursor, started.cursor);
	assert.deepEqual(upgraded.eventsAfter(user.cursor).events, [started, finished]);
	assert.deepEqual(upgraded.events(), fixture);
	assert.deepEqual(persisted, fixture);
});

test("repeated mixed-version hydration preserves every identity and rejects cross-version conflicts", () => {
	const persisted = structuredClone(mixedReplayEvents());
	const exactMixedJournal = structuredClone(persisted);
	const v2Head = exactMixedJournal[1];
	assert.ok(v2Head);
	const branch = () => persisted.map((event, index) => customEntry(`restart-${index}`, event));

	const firstRestart = DurableSessionLog.hydrate({
		binding: mixedBinding,
		activeBranch: branch(),
		appendEntry() {
			throw new Error("restart hydration must not append");
		},
		appendKind: "nopal.session.event/v3",
	});
	const secondRestart = DurableSessionLog.hydrate({
		binding: mixedBinding,
		activeBranch: branch(),
		appendEntry() {
			throw new Error("second restart hydration must not append");
		},
		appendKind: "nopal.session.event/v3",
	});

	assert.deepEqual(firstRestart.events(), exactMixedJournal);
	assert.deepEqual(secondRestart.events(), exactMixedJournal);
	assert.equal(firstRestart.eventCount, 4);
	assert.equal(secondRestart.eventCount, 4);
	assert.deepEqual(firstRestart.eventsAfter(v2Head.cursor).events, exactMixedJournal.slice(2));
	assert.deepEqual(secondRestart.eventsAfter(v2Head.cursor).events, exactMixedJournal.slice(2));
	assert.deepEqual(firstRestart.eventsAfter(firstRestart.headCursor!).events, []);
	assert.deepEqual(secondRestart.eventsAfter(secondRestart.headCursor!).events, []);
	expectCode(
		() => DurableSessionLog.hydrate({
			binding: mixedBinding,
			activeBranch: branch(),
			appendEntry() {},
		}),
		"malformed_history",
	);

	expectCode(
		() => firstRestart.append({
			eventId: "event-ready",
			event: {
				activity_id: "activity-duplicate",
				command: "duplicate",
				started_at: "2026-07-13T12:00:01Z",
				tool_call_id: "tool-call-duplicate",
				type: "command_started",
			},
		}),
		"duplicate_event",
	);
	const foreignSuffix = structuredClone(persisted);
	foreignSuffix[2]!.session_id = "session-foreign";
	expectCode(
		() => DurableSessionLog.hydrate({
			binding: mixedBinding,
			activeBranch: foreignSuffix.map((event, index) => customEntry(`foreign-${index}`, event)),
			appendEntry() {},
			appendKind: "nopal.session.event/v3",
		}),
		"foreign_history",
	);
	const brokenChain = structuredClone(persisted);
	brokenChain[3]!.previous_cursor = v2Head.cursor;
	expectCode(
		() => DurableSessionLog.hydrate({
			binding: mixedBinding,
			activeBranch: brokenChain.map((event, index) => customEntry(`broken-${index}`, event)),
			appendEntry() {},
			appendKind: "nopal.session.event/v3",
		}),
		"history_corrupt",
	);
	const orphanCommand = DurableSessionLog.hydrate({
		binding: mixedBinding,
		activeBranch: [customEntry("ready-only", exactMixedJournal[0])],
		appendEntry() {},
		appendKind: "nopal.session.event/v3",
	});
	expectCode(
		() => orphanCommand.append({
			eventId: "event-orphan-command-v3",
			commandId: "command-missing",
			event: {
				activity_id: "activity-orphan",
				command: "false",
				started_at: "2026-07-13T12:00:02Z",
				tool_call_id: "tool-call-orphan",
				type: "command_started",
			},
		}),
		"history_corrupt",
	);
});

test("v3 validation covers the complete frozen activity surface without raw tool payloads", () => {
	const persisted: DurableSessionEvent[] = [];
	const log = DurableSessionLog.hydrate({
		binding,
		activeBranch: [],
		appendEntry(_customType, data) {
			persisted.push(data);
		},
		appendKind: "nopal.session.event/v3",
	});
	const events: DurableSessionEventPayload[] = [
		{
			activity_id: "activity-command-finished-without-output",
			duration_ms: 4,
			exit: { reason: "exit facts unavailable", type: "unavailable" },
			outcome: "unknown",
			tool_call_id: "tool-call-command-finished-without-output",
			type: "command_finished",
		},
		{
			activity_id: "activity-command-failed",
			message: "command could not start",
			tool_call_id: "tool-call-command-failed",
			type: "command_failed",
		},
		{
			activity_id: "activity-tool-started",
			started_at: "2026-07-13T12:00:03Z",
			summary: {
				details_unavailable: true,
				omitted_bytes: 0,
				original_bytes: 19,
				text: "Details unavailable",
				truncated: false,
			},
			tool_call_id: "tool-call-tool-started",
			tool_name: "unknown-tool",
			type: "tool_started",
		},
		{
			activity_id: "activity-tool-finished",
			duration_ms: 12,
			outcome: "succeeded",
			summary: {
				details_unavailable: false,
				omitted_bytes: 0,
				original_bytes: 2,
				text: "ok",
				truncated: false,
			},
			tool_call_id: "tool-call-tool-finished",
			type: "tool_finished",
		},
		{
			activity_id: "activity-tool-failed",
			message: "tool failed safely",
			outcome: "failed",
			tool_call_id: "tool-call-tool-failed",
			type: "tool_failed",
		},
	];

	for (const [index, event] of events.entries()) {
		const appended = log.append({ eventId: `event-v3-surface-${index}`, event });
		assert.equal(appended.kind, "nopal.session.event/v3");
		assert.deepEqual(appended.event, event);
	}
	assert.equal(persisted.length, events.length);
	assert.equal("input" in persisted[2]!.event, false);
	assert.equal("result" in persisted[2]!.event, false);

	expectCode(
		() => log.append({
			eventId: "event-v3-oversized-tool-name",
			event: {
				activity_id: "activity-oversized-tool-name",
				started_at: "2026-07-13T12:00:04Z",
				summary: {
					details_unavailable: true,
					omitted_bytes: 0,
					original_bytes: 19,
					text: "Details unavailable",
					truncated: false,
				},
				tool_call_id: "tool-call-oversized-tool-name",
				tool_name: "x".repeat(257),
				type: "tool_started",
			},
		}),
		"malformed_history",
	);
	expectCode(
		() => log.append({
			eventId: "event-v3-bad-tool-summary",
			event: {
				activity_id: "activity-bad-tool-summary",
				started_at: "2026-07-13T12:00:05Z",
				summary: {
					details_unavailable: true,
					omitted_bytes: 1,
					original_bytes: 19,
					text: "Details unavailable",
					truncated: false,
				},
				tool_call_id: "tool-call-bad-tool-summary",
				tool_name: "unknown-tool",
				type: "tool_started",
			},
		}),
		"malformed_history",
	);
	expectCode(
		() => log.append({
			eventId: "event-v3-missing-failure-outcome",
			event: {
				activity_id: "activity-missing-failure-outcome",
				message: "tool failed safely",
				tool_call_id: "tool-call-missing-failure-outcome",
				type: "tool_failed",
			},
		}),
		"malformed_history",
	);
	expectCode(
		() => log.append({
			eventId: "event-v3-raw-tool-input",
			event: {
				activity_id: "activity-raw-tool-input",
				input: { secret: "must-not-persist" },
				started_at: "2026-07-13T12:00:06Z",
				summary: {
					details_unavailable: true,
					omitted_bytes: 0,
					original_bytes: 19,
					text: "Details unavailable",
					truncated: false,
				},
				tool_call_id: "tool-call-raw-tool-input",
				tool_name: "unknown-tool",
				type: "tool_started",
			},
		}),
		"malformed_history",
	);
	expectCode(
		() => log.append({
			eventId: "event-v3-raw-tool-result",
			event: {
				activity_id: "activity-raw-tool-result",
				duration_ms: 1,
				outcome: "succeeded",
				result: { secret: "must-not-persist" },
				summary: {
					details_unavailable: false,
					omitted_bytes: 0,
					original_bytes: 2,
					text: "ok",
					truncated: false,
				},
				tool_call_id: "tool-call-raw-tool-result",
				type: "tool_finished",
			},
		}),
		"malformed_history",
	);
	expectCode(
		() => log.append({
			eventId: "event-v3-orphan-tool-command",
			commandId: "command-missing",
			event: {
				activity_id: "activity-orphan-tool-command",
				message: "tool failed safely",
				outcome: "failed",
				tool_call_id: "tool-call-orphan-tool-command",
				type: "tool_failed",
			},
		}),
		"history_corrupt",
	);
});

test("canonical cursor material rejects non-JSON values and ignores object key order", () => {
	const left = hydrate([]);
	const right = hydrate([]);
	const leftEvent = left.append({
		eventId: "event-ready",
		commandId: "command-01",
		event: { type: "session_ready", nested: { z: 2, a: 1 } },
	});
	const rightEvent = right.append({
		eventId: "event-ready",
		commandId: "command-01",
		event: { nested: { a: 1, z: 2 }, type: "session_ready" },
	});
	assert.equal(leftEvent.cursor, rightEvent.cursor);

	const invalid = hydrate([]);
	expectCode(
		() => invalid.append({ eventId: "event-nan", event: { type: "session_ready", value: Number.NaN } }),
		"malformed_history",
	);
	expectCode(
		() => invalid.append({ eventId: "event-bigint", event: { type: "session_ready", value: 1n } }),
		"malformed_history",
	);
	expectCode(
		() => invalid.append({ eventId: "event-undefined", event: { type: "session_ready", value: undefined } }),
		"malformed_history",
	);
	const cyclic: Record<string, unknown> = { type: "session_ready" };
	cyclic.self = cyclic;
	expectCode(() => invalid.append({ eventId: "event-cycle", event: cyclic }), "malformed_history");
	assert.equal(invalid.eventCount, 0);

	const hostile = JSON.parse('{"type":"session_ready","__proto__":{"polluted":true}}') as Record<string, unknown>;
	const safe = invalid.append({ eventId: "event-hostile-key", event: hostile });
	assert.equal(Object.hasOwn(safe.event, "__proto__"), true);
	assert.equal(({} as { polluted?: boolean }).polluted, undefined);
});

test("preserves the common prefix and rejects a cursor on an abandoned Pi branch", () => {
	const persisted: DurableSessionEvent[] = [];
	const original = hydrate([], (_customType, data) => persisted.push(data as DurableSessionEvent));
	const first = original.append({ eventId: "event-1", event: { type: "session_ready" } });
	const common = original.append({
		eventId: "event-2",
		commandId: "command-01",
		event: { type: "user_message", text: "Question" },
	});
	const abandoned = original.append({
		eventId: "event-3",
		commandId: "command-01",
		event: { type: "assistant_message", text: "Old answer" },
	});

	const branched = hydrate(
		persisted.slice(0, 2).map((event, index) => customEntry(`branch-${index}`, event)),
		() => undefined,
		undefined,
		[abandoned.cursor],
	);
	assert.deepEqual(branched.events().map((event) => event.cursor), [first.cursor, common.cursor]);
	assert.deepEqual(branched.eventsAfter(common.cursor, 8).events, []);
	expectCode(() => branched.eventsAfter(abandoned.cursor, 8), "branch_divergence");

	const replacement = branched.append({
		eventId: "event-4",
		commandId: "command-01",
		event: { type: "assistant_message", text: "New answer" },
	});
	assert.equal(replacement.previous_cursor, common.cursor);
	assert.notEqual(replacement.cursor, abandoned.cursor);
});

test("reuses an existing ready event and commits a new event before indexing it", () => {
	const persisted: Array<{ customType: string; data: unknown }> = [];
	let log: DurableSessionLog;
	log = hydrate([], (customType, data) => {
		assert.equal(log.eventCount, persisted.length, "append callback is the persistence commit point");
		persisted.push({ customType, data });
	});

	const ready = log.ensureReady("event-ready");
	assert.equal(log.ensureReady("unused-id"), ready);
	assert.equal(log.eventCount, 1);
	assert.equal(persisted.length, 1);
	assert.deepEqual(persisted[0], { customType: SESSION_EVENT_ENTRY, data: ready });

	const user = log.append({
		eventId: "event-user",
		commandId: "command-01",
		event: { type: "user_message", text: "Inspect" },
	});
	assert.equal(log.eventCount, 2);
	assert.equal(user.previous_cursor, ready.cursor);
	assert.equal(log.classifyCommand("command-01", "Inspect").kind, "duplicate");
});

test("does not mutate indexes when persistence rejects an append", () => {
	const ready = legacyEvent("event-existing-ready", { type: "session_ready" });
	const log = hydrate([customEntry("existing-ready", ready)], () => {
		throw new Error("disk full");
	});
	const beforeEvents = log.events();
	const beforeHead = log.headCursor;
	const beforeBytes = log.byteCount;

	const error = expectCode(
		() => log.append({
			eventId: "event-user",
			commandId: "command-new",
			event: { type: "user_message", text: "Never committed" },
		}),
		"persistence_failed",
	);
	assert.match(error.message, /disk full/u);
	assert.equal(log.eventCount, 1);
	assert.equal(log.headCursor, beforeHead);
	assert.equal(log.byteCount, beforeBytes);
	assert.deepEqual(log.events(), beforeEvents);
	assert.equal(log.ready()?.event_id, "event-existing-ready");
	assert.equal(log.classifyCommand("command-new", "Never committed").kind, "new");
});

test("pages from genesis or a known cursor and classifies unsafe resume cursors", () => {
	const log = hydrate([]);
	const first = log.append({ eventId: "event-1", event: { type: "session_ready" } });
	const second = log.append({
		eventId: "event-2",
		commandId: "command-01",
		event: { type: "user_message", text: "Question" },
	});
	const third = log.append({
		eventId: "event-3",
		commandId: "command-01",
		event: { type: "assistant_message", text: "Answer" },
	});

	assert.deepEqual(log.eventsAfter(null, 2), {
		fromCursor: null,
		events: [first, second],
		nextCursor: second.cursor,
		headCursor: third.cursor,
		hasMore: true,
	});
	assert.deepEqual(log.eventsAfter(second.cursor, 2), {
		fromCursor: second.cursor,
		events: [third],
		nextCursor: third.cursor,
		headCursor: third.cursor,
		hasMore: false,
	});
	assert.deepEqual(log.eventsAfter(third.cursor, 2).events, []);

	expectCode(() => log.eventsAfter("not-a-cursor", 2), "malformed_cursor");
	const foreign = third.cursor.replace(log.streamId.slice(-64), "f".repeat(64));
	expectCode(() => log.eventsAfter(foreign, 2), "foreign_cursor");
	const fabricated = third.cursor.replace(/[0-9a-f]{64}$/u, "a".repeat(64));
	expectCode(() => log.eventsAfter(fabricated, 2), "history_gap");
});

test("fails closed on foreign, malformed, duplicate, or divergent active history", () => {
	const foreign = legacyEvent("event-foreign", { type: "session_ready" });
	foreign.session_id = "session-02";
	expectCode(() => hydrate([customEntry("foreign", foreign)]), "foreign_history");

	expectCode(
		() => hydrate([customEntry("malformed", { kind: SESSION_EVENT_V1_KIND, event_id: "event-bad" })]),
		"malformed_history",
	);

	const duplicate = legacyEvent("event-duplicate", { type: "session_ready" });
	expectCode(
		() => hydrate([customEntry("one", duplicate), customEntry("two", duplicate)]),
		"duplicate_event",
	);

	const source = hydrate([]);
	const valid = source.append({ eventId: "event-one", event: { type: "session_ready" } });
	const divergent = { ...valid, previous_cursor: "nopal.session.cursor/v1:wrong" };
	expectCode(() => hydrate([customEntry("divergent", divergent)]), "history_corrupt");

	const foreignPersisted: DurableSessionEvent[] = [];
	const foreignLog = DurableSessionLog.hydrate({
		binding: { plotId: "plot-foreign", sessionId: "session-foreign" },
		activeBranch: [],
		appendEntry(_customType, data) {
			foreignPersisted.push(data);
		},
	});
	foreignLog.append({ eventId: "foreign-v2", event: { type: "session_ready" } });
	expectCode(() => hydrate([customEntry("foreign-v2", foreignPersisted[0])]), "foreign_history");

	const malformedV2 = { ...valid } as Record<string, unknown>;
	delete malformedV2.cursor;
	expectCode(() => hydrate([customEntry("malformed-v2", malformedV2)]), "history_corrupt");

	expectCode(
		() => hydrate([{ type: "message", customType: SESSION_EVENT_ENTRY, data: duplicate }]),
		"malformed_history",
	);
});

test("enforces event, history, and replay bounds without truncation", () => {
	const eventLimited = hydrate([], () => undefined, { maxEvents: 1 });
	eventLimited.append({ eventId: "event-1", event: { type: "session_ready" } });
	expectCode(
		() => eventLimited.append({ eventId: "event-2", event: { type: "assistant_message", text: "extra" } }),
		"history_too_large",
	);
	assert.equal(eventLimited.eventCount, 1);

	const lineLimited = hydrate([], () => undefined, { maxEventBytes: 220 });
	expectCode(
		() => lineLimited.append({ eventId: "event-large", event: { type: "assistant_message", text: "x".repeat(500) } }),
		"event_too_large",
	);
	assert.equal(lineLimited.eventCount, 0);

	const source = hydrate([]);
	const event = source.append({ eventId: "event-ready", event: { type: "session_ready" } });
	expectCode(
		() => hydrate([customEntry("too-many", event)], () => undefined, { maxBytes: 1 }),
		"history_too_large",
	);

	const replayLimited = hydrate([], () => undefined, { maxReplayPageEvents: 1 });
	replayLimited.append({ eventId: "event-ready", event: { type: "session_ready" } });
	expectCode(() => replayLimited.eventsAfter(null, 2), "invalid_limit");
});

test("injected limits may lower but never raise frozen hard ceilings", () => {
	const cases = [
		{ maxEvents: DEFAULT_MAX_DURABLE_SESSION_EVENTS + 1 },
		{ maxBytes: DEFAULT_MAX_DURABLE_SESSION_BYTES + 1 },
		{ maxEventBytes: DEFAULT_MAX_DURABLE_SESSION_EVENT_BYTES + 1 },
		{ maxReplayPageEvents: DEFAULT_MAX_REPLAY_PAGE_EVENTS + 1 },
	];
	for (const limits of cases) {
		expectCode(() => hydrate([], () => undefined, limits), "invalid_limit");
	}
});

test("requires command causality before persistence or index mutation", () => {
	const persisted: DurableSessionEvent[] = [];
	const log = hydrate([], (_customType, data) => persisted.push(data as DurableSessionEvent));
	log.append({ eventId: "event-ready", event: { type: "session_ready" } });
	const before = log.events();

	expectCode(
		() => log.append({ eventId: "event-user", event: { type: "user_message", text: "Missing identity" } }),
		"history_corrupt",
	);
	expectCode(
		() => log.append({
			eventId: "event-assistant",
			commandId: "command-missing",
			event: { type: "assistant_message", text: "No user command" },
		}),
		"history_corrupt",
	);
	expectCode(
		() => log.append({
			eventId: "event-error",
			commandId: "command-missing",
			event: { type: "session_error", message: "No user command" },
		}),
		"history_corrupt",
	);
	assert.deepEqual(log.events(), before);
	assert.equal(persisted.length, 1);

	log.append({
		eventId: "event-user-valid",
		commandId: "command-valid",
		event: { type: "user_message", text: "Continue" },
	});
	log.append({
		eventId: "event-assistant-one",
		commandId: "command-valid",
		event: { type: "assistant_message", text: "First tool-loop result" },
	});
	log.append({
		eventId: "event-assistant-two",
		commandId: "command-valid",
		event: { type: "assistant_message", text: "Final result" },
	});
	assert.equal(log.eventCount, 4);
});

test("rejects command-causality corruption while hydrating persisted history", () => {
	const missingCommand = legacyEvent("event-user", { type: "user_message", text: "Missing identity" });
	expectCode(() => hydrate([customEntry("missing-command", missingCommand)]), "history_corrupt");

	const orphanAssistant = legacyEvent(
		"event-assistant",
		{ type: "assistant_message", text: "No user command" },
		"command-orphan",
	);
	expectCode(() => hydrate([customEntry("orphan-assistant", orphanAssistant)]), "history_corrupt");

	const orphanError = legacyEvent(
		"event-error",
		{ type: "session_error", message: "No user command" },
		"command-orphan",
	);
	expectCode(() => hydrate([customEntry("orphan-error", orphanError)]), "history_corrupt");

	const validHistory = [
		customEntry("user", legacyEvent(
			"event-user-valid",
			{ type: "user_message", text: "Continue" },
			"command-valid",
		)),
		customEntry("assistant-one", legacyEvent(
			"event-assistant-one",
			{ type: "assistant_message", text: "First tool-loop result" },
			"command-valid",
		)),
		customEntry("assistant-two", legacyEvent(
			"event-assistant-two",
			{ type: "assistant_message", text: "Final result" },
			"command-valid",
		)),
	];
	assert.equal(hydrate(validHistory).eventCount, 3);
});

test("rejects conflicting command content and preserves exact duplicate identity", () => {
	const log = hydrate([]);
	const user = log.append({
		eventId: "event-user",
		commandId: "command-01",
		event: { type: "user_message", text: "Same" },
	});

	assert.deepEqual(log.classifyCommand("command-01", "Same"), { kind: "duplicate", event: user });
	expectCode(() => log.classifyCommand("command-01", "Changed"), "command_conflict");
	expectCode(
		() => log.append({
			eventId: "event-user-2",
			commandId: "command-01",
			event: { type: "user_message", text: "Same" },
		}),
		"duplicate_command_event",
	);
});

test("returns immutable snapshots so callers cannot corrupt cursor indexes", () => {
	const log = hydrate([]);
	const event = log.append({ eventId: "event-ready", event: { type: "session_ready" } });
	assert.ok(Object.isFrozen(event));
	assert.ok(Object.isFrozen(event.event));
	assert.throws(() => ((event as unknown as { cursor: string }).cursor = "changed"), TypeError);
	assert.throws(() => (log.events() as DurableSessionEvent[]).push(event), TypeError);
	assert.equal(log.headCursor, event.cursor);
});
