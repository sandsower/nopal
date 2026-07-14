import { createHash } from "node:crypto";

export const SESSION_EVENT_ENTRY = "nopal-session-event" as const;
export const SESSION_EVENT_V1_KIND = "nopal.session.event/v1" as const;
export const SESSION_EVENT_V2_KIND = "nopal.session.event/v2" as const;
export const SESSION_EVENT_V3_KIND = "nopal.session.event/v3" as const;
export const SESSION_STREAM_PREFIX = "nopal.session.stream/v1:" as const;
export const SESSION_CURSOR_PREFIX = "nopal.session.cursor/v1:" as const;

export const DEFAULT_MAX_DURABLE_SESSION_EVENTS = 100_000;
export const DEFAULT_MAX_DURABLE_SESSION_BYTES = 256 * 1024 * 1024;
export const DEFAULT_MAX_DURABLE_SESSION_EVENT_BYTES = 1024 * 1024;
export const DEFAULT_MAX_REPLAY_PAGE_EVENTS = 1024;

export const MAX_SESSION_IDENTITY_BYTES = 4096;
export const MAX_ACTIVITY_TOOL_NAME_BYTES = 256;
export const MAX_ACTIVITY_DISPLAY_BYTES = 8192;
export const MAX_ACTIVITY_FAILURE_BYTES = 4096;
export const MAX_ACTIVITY_OUTPUT_BYTES = 32768;
const GENESIS = "<genesis>";
const CURSOR_PATTERN = /^nopal\.session\.cursor\/v1:([0-9a-f]{64}):([1-9][0-9]*):([0-9a-f]{64})$/u;

export type JsonPrimitive = null | boolean | number | string;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export type SessionBinding = {
	plotId: string;
	sessionId: string;
};

export type DurableSessionEventPayload = {
	type:
		| "session_ready"
		| "user_message"
		| "assistant_message"
		| "session_error"
		| "command_started"
		| "command_finished"
		| "command_failed"
		| "tool_started"
		| "tool_finished"
		| "tool_failed";
	[key: string]: JsonValue;
};

export type DurableSessionEventKind =
	| typeof SESSION_EVENT_V2_KIND
	| typeof SESSION_EVENT_V3_KIND;

export type DurableSessionEvent = {
	kind: DurableSessionEventKind;
	event_id: string;
	plot_id: string;
	session_id: string;
	stream_id: string;
	sequence: number;
	previous_cursor: string | null;
	cursor: string;
	command_id?: string;
	event: DurableSessionEventPayload;
	[key: string]: JsonValue | undefined;
};

export type LegacySessionEvent = {
	kind: typeof SESSION_EVENT_V1_KIND;
	event_id: string;
	plot_id: string;
	session_id: string;
	command_id?: string;
	event: DurableSessionEventPayload;
	[key: string]: JsonValue | undefined;
};

export type PiSessionEntry = {
	type?: unknown;
	id?: unknown;
	parentId?: unknown;
	customType?: unknown;
	data?: unknown;
	[key: string]: unknown;
};

export type DurableSessionLogLimits = {
	maxEvents?: number;
	maxBytes?: number;
	maxEventBytes?: number;
	maxReplayPageEvents?: number;
};

export type DurableSessionLogOptions = {
	binding: SessionBinding;
	activeBranch: readonly PiSessionEntry[];
	appendEntry(customType: typeof SESSION_EVENT_ENTRY, data: DurableSessionEvent): void;
	appendKind?: DurableSessionEventKind;
	limits?: DurableSessionLogLimits;
	abandonedCursors?: readonly string[];
};

export type AppendSessionEvent = {
	eventId: string;
	commandId?: string;
	event: unknown;
	extra?: Record<string, unknown>;
};

export type CommandDisposition =
	| { kind: "new"; fingerprint: string }
	| { kind: "duplicate"; event: DurableSessionEvent };

export type SessionReplaySlice = {
	fromCursor: string | null;
	events: readonly DurableSessionEvent[];
	nextCursor: string | null;
	headCursor: string | null;
	hasMore: boolean;
};

export type DurableSessionLogErrorCode =
	| "invalid_binding"
	| "malformed_history"
	| "history_corrupt"
	| "history_gap"
	| "foreign_history"
	| "duplicate_event"
	| "duplicate_command_event"
	| "command_conflict"
	| "branch_divergence"
	| "history_too_large"
	| "event_too_large"
	| "persistence_failed"
	| "malformed_cursor"
	| "foreign_cursor"
	| "invalid_limit";

export class DurableSessionLogError extends Error {
	readonly code: DurableSessionLogErrorCode;

	constructor(code: DurableSessionLogErrorCode, message: string, options?: { cause?: unknown }) {
		super(message, options);
		this.name = "DurableSessionLogError";
		this.code = code;
	}
}

type ResolvedLimits = Required<DurableSessionLogLimits>;

type SeenCommand = {
	fingerprint: string;
	event: DurableSessionEvent;
};

function fail(code: DurableSessionLogErrorCode, message: string): never {
	throw new DurableSessionLogError(code, message);
}

function isObject(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function safeIdentity(value: unknown): value is string {
	return typeof value === "string"
		&& value.trim().length > 0
		&& Buffer.byteLength(value, "utf8") <= MAX_SESSION_IDENTITY_BYTES
		&& !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function requireIdentity(value: unknown, field: string, code: DurableSessionLogErrorCode): string {
	if (!safeIdentity(value)) fail(code, `invalid durable Session ${field}`);
	return value;
}

function boundedPositiveInteger(
	value: number | undefined,
	fallback: number,
	field: string,
	hardMaximum: number,
): number {
	const resolved = value ?? fallback;
	if (!Number.isSafeInteger(resolved) || resolved <= 0) {
		fail("invalid_limit", `${field} must be a positive safe integer`);
	}
	if (resolved > hardMaximum) {
		fail("invalid_limit", `${field} cannot exceed the frozen hard maximum ${hardMaximum}`);
	}
	return resolved;
}

function resolveLimits(limits: DurableSessionLogLimits | undefined): ResolvedLimits {
	return {
		maxEvents: boundedPositiveInteger(
			limits?.maxEvents,
			DEFAULT_MAX_DURABLE_SESSION_EVENTS,
			"maxEvents",
			DEFAULT_MAX_DURABLE_SESSION_EVENTS,
		),
		maxBytes: boundedPositiveInteger(
			limits?.maxBytes,
			DEFAULT_MAX_DURABLE_SESSION_BYTES,
			"maxBytes",
			DEFAULT_MAX_DURABLE_SESSION_BYTES,
		),
		maxEventBytes: boundedPositiveInteger(
			limits?.maxEventBytes,
			DEFAULT_MAX_DURABLE_SESSION_EVENT_BYTES,
			"maxEventBytes",
			DEFAULT_MAX_DURABLE_SESSION_EVENT_BYTES,
		),
		maxReplayPageEvents: boundedPositiveInteger(
			limits?.maxReplayPageEvents,
			DEFAULT_MAX_REPLAY_PAGE_EVENTS,
			"maxReplayPageEvents",
			DEFAULT_MAX_REPLAY_PAGE_EVENTS,
		),
	};
}

function resolveAppendKind(kind: DurableSessionEventKind | undefined): DurableSessionEventKind {
	const resolved = kind ?? SESSION_EVENT_V2_KIND;
	if (resolved !== SESSION_EVENT_V2_KIND && resolved !== SESSION_EVENT_V3_KIND) {
		fail("invalid_binding", `unsupported durable Session append kind ${JSON.stringify(resolved)}`);
	}
	return resolved;
}

function sha256(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function defineJsonField(target: Record<string, JsonValue>, key: string, value: JsonValue): void {
	Object.defineProperty(target, key, {
		value,
		enumerable: true,
		configurable: true,
		writable: true,
	});
}

function normalizeJson(value: unknown, path: string, ancestors = new Set<object>()): JsonValue {
	if (value === null || typeof value === "string" || typeof value === "boolean") return value;
	if (typeof value === "number") {
		if (!Number.isFinite(value)) fail("malformed_history", `${path} contains a non-finite number`);
		return value;
	}
	if (typeof value !== "object") fail("malformed_history", `${path} is not JSON data`);
	if (ancestors.has(value)) fail("malformed_history", `${path} contains a cycle`);
	ancestors.add(value);
	try {
		if (Array.isArray(value)) {
			return value.map((item, index) => normalizeJson(item, `${path}[${index}]`, ancestors));
		}
		const prototype = Object.getPrototypeOf(value);
		if (prototype !== Object.prototype && prototype !== null) {
			fail("malformed_history", `${path} is not a plain JSON object`);
		}
		const normalized: Record<string, JsonValue> = {};
		for (const key of Object.keys(value).sort()) {
			defineJsonField(
				normalized,
				key,
				normalizeJson((value as Record<string, unknown>)[key], `${path}.${key}`, ancestors),
			);
		}
		return normalized;
	} finally {
		ancestors.delete(value);
	}
}

function canonicalJson(value: unknown, path: string): string {
	return JSON.stringify(normalizeJson(value, path));
}

function deepFreeze<T>(value: T): T {
	if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
		for (const child of Object.values(value as Record<string, unknown>)) deepFreeze(child);
		Object.freeze(value);
	}
	return value;
}

function requirePayloadString(
	value: Record<string, unknown>,
	field: string,
	type: string,
	code: DurableSessionLogErrorCode,
): string {
	const fieldValue = value[field];
	if (typeof fieldValue !== "string") fail(code, `durable Session ${type} requires ${field}`);
	return fieldValue;
}

function requireBoundedPayloadString(
	value: Record<string, unknown>,
	field: string,
	type: string,
	maxBytes: number,
	code: DurableSessionLogErrorCode,
): string {
	const fieldValue = requirePayloadString(value, field, type, code);
	if (
		fieldValue.trim().length === 0
		|| Buffer.byteLength(fieldValue, "utf8") > maxBytes
		|| /[\u0000-\u001f\u007f-\u009f]/u.test(fieldValue)
	) {
		fail(code, `durable Session ${type} has invalid ${field}`);
	}
	return fieldValue;
}

function requireNonNegativeInteger(
	value: Record<string, unknown>,
	field: string,
	type: string,
	code: DurableSessionLogErrorCode,
): number {
	const fieldValue = value[field];
	if (typeof fieldValue !== "number" || !Number.isSafeInteger(fieldValue) || fieldValue < 0) {
		fail(code, `durable Session ${type} requires a non-negative ${field}`);
	}
	return fieldValue;
}

function validateOptionalNonNegativeInteger(
	value: Record<string, unknown>,
	field: string,
	type: string,
	code: DurableSessionLogErrorCode,
): void {
	if (value[field] !== undefined && value[field] !== null) {
		requireNonNegativeInteger(value, field, type, code);
	}
}

function validateV2Payload(value: Record<string, unknown>, code: DurableSessionLogErrorCode): boolean {
	switch (value.type) {
		case "session_ready":
			return true;
		case "user_message":
		case "assistant_message":
			requirePayloadString(value, "text", value.type, code);
			return true;
		case "session_error":
			requirePayloadString(value, "message", value.type, code);
			return true;
		default:
			return false;
	}
}

function validateCommandExit(value: unknown, code: DurableSessionLogErrorCode): void {
	if (!isObject(value) || typeof value.type !== "string") {
		fail(code, "durable Session command_finished requires a tagged exit outcome");
	}
	switch (value.type) {
		case "code":
			if (
				typeof value.code !== "number"
				|| !Number.isSafeInteger(value.code)
				|| value.code < -2_147_483_648
				|| value.code > 2_147_483_647
			) {
				fail(code, "durable Session command_finished exit code must be a signed 32-bit integer");
			}
			return;
		case "signal":
			requireBoundedPayloadString(
				value,
				"signal",
				"command_finished exit",
				MAX_ACTIVITY_FAILURE_BYTES,
				code,
			);
			return;
		case "unavailable":
			requireBoundedPayloadString(
				value,
				"reason",
				"command_finished exit",
				MAX_ACTIVITY_FAILURE_BYTES,
				code,
			);
			return;
		default:
			fail(code, `unsupported durable Session command exit type ${JSON.stringify(value.type)}`);
	}
}

function validateBoundedTextFacts(
	value: Record<string, unknown>,
	maxBytes: number,
	type: string,
	code: DurableSessionLogErrorCode,
): void {
	const text = requirePayloadString(value, "text", type, code);
	if (typeof value.truncated !== "boolean") fail(code, `durable Session ${type} requires truncated`);
	const originalBytes = requireNonNegativeInteger(value, "original_bytes", type, code);
	const omittedBytes = requireNonNegativeInteger(value, "omitted_bytes", type, code);
	const retainedBytes = Buffer.byteLength(text, "utf8");
	if (
		retainedBytes > maxBytes
		|| originalBytes - omittedBytes !== retainedBytes
		|| (value.truncated ? omittedBytes === 0 : omittedBytes !== 0)
	) {
		fail(code, `durable Session ${type} has inconsistent bounded-text facts`);
	}
}

function validateCommandOutput(value: unknown, code: DurableSessionLogErrorCode): void {
	if (value === undefined || value === null) return;
	if (!isObject(value)) fail(code, "durable Session command_finished output is malformed");
	if (!["stdout", "stderr", "combined"].includes(String(value.channel))) {
		fail(code, "durable Session command_finished output channel is unsupported");
	}
	validateBoundedTextFacts(value, MAX_ACTIVITY_OUTPUT_BYTES, "command_finished output", code);
}

function validateActivitySummary(value: unknown, code: DurableSessionLogErrorCode): void {
	if (!isObject(value)) fail(code, "durable Session tool activity summary is malformed");
	if (value.details_unavailable !== undefined && typeof value.details_unavailable !== "boolean") {
		fail(code, "durable Session tool activity summary has invalid details_unavailable");
	}
	validateBoundedTextFacts(value, MAX_ACTIVITY_DISPLAY_BYTES, "tool activity summary", code);
}

function rejectRawToolPayload(value: Record<string, unknown>, code: DurableSessionLogErrorCode): void {
	for (const field of ["input", "arguments", "result", "raw_input", "raw_result"]) {
		if (Object.hasOwn(value, field)) {
			fail(code, `durable Session tool activity must not persist raw ${field}`);
		}
	}
}

function isV3ActivityEventType(type: DurableSessionEventPayload["type"]): boolean {
	switch (type) {
		case "command_started":
		case "command_finished":
		case "command_failed":
		case "tool_started":
		case "tool_finished":
		case "tool_failed":
			return true;
		default:
			return false;
	}
}

function validateV3Payload(value: Record<string, unknown>, code: DurableSessionLogErrorCode): void {
	if (validateV2Payload(value, code)) return;
	switch (value.type) {
		case "command_started":
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			requireBoundedPayloadString(value, "command", value.type, MAX_ACTIVITY_DISPLAY_BYTES, code);
			requireBoundedPayloadString(value, "started_at", value.type, MAX_ACTIVITY_DISPLAY_BYTES, code);
			if (value.working_directory !== undefined && value.working_directory !== null) {
				requireBoundedPayloadString(
					value,
					"working_directory",
					value.type,
					MAX_ACTIVITY_DISPLAY_BYTES,
					code,
				);
			}
			return;
		case "command_finished":
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			requireNonNegativeInteger(value, "duration_ms", value.type, code);
			validateCommandExit(value.exit, code);
			if (!["succeeded", "failed", "cancelled", "unknown"].includes(String(value.outcome))) {
				fail(code, "durable Session command_finished outcome is unsupported");
			}
			validateCommandOutput(value.output, code);
			return;
		case "command_failed":
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			validateOptionalNonNegativeInteger(value, "duration_ms", value.type, code);
			requireBoundedPayloadString(value, "message", value.type, MAX_ACTIVITY_FAILURE_BYTES, code);
			return;
		case "tool_started":
			rejectRawToolPayload(value, code);
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			requireBoundedPayloadString(value, "tool_name", value.type, MAX_ACTIVITY_TOOL_NAME_BYTES, code);
			validateActivitySummary(value.summary, code);
			requireBoundedPayloadString(value, "started_at", value.type, MAX_ACTIVITY_DISPLAY_BYTES, code);
			return;
		case "tool_finished":
			rejectRawToolPayload(value, code);
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			requireNonNegativeInteger(value, "duration_ms", value.type, code);
			if (!["succeeded", "cancelled", "unknown"].includes(String(value.outcome))) {
				fail(code, "durable Session tool_finished outcome is unsupported");
			}
			validateActivitySummary(value.summary, code);
			return;
		case "tool_failed":
			rejectRawToolPayload(value, code);
			requireIdentity(value.activity_id, "activity_id", code);
			requireIdentity(value.tool_call_id, "tool_call_id", code);
			validateOptionalNonNegativeInteger(value, "duration_ms", value.type, code);
			requireBoundedPayloadString(value, "message", value.type, MAX_ACTIVITY_FAILURE_BYTES, code);
			if (value.outcome !== "failed") {
				fail(code, "durable Session tool_failed outcome must be failed");
			}
			return;
		default:
			fail(code, `unsupported durable Session event type ${JSON.stringify(value.type)}`);
	}
}

function normalizePayload(
	value: unknown,
	kind: DurableSessionEventKind,
	code: DurableSessionLogErrorCode,
): DurableSessionEventPayload {
	if (!isObject(value) || typeof value.type !== "string") fail(code, "durable Session event payload is malformed");
	if (kind === SESSION_EVENT_V2_KIND) {
		if (!validateV2Payload(value, code)) {
			fail(code, `unsupported durable Session v2 event type ${JSON.stringify(value.type)}`);
		}
	} else {
		validateV3Payload(value, code);
	}
	let normalized: JsonValue;
	try {
		normalized = normalizeJson(value, "event");
	} catch (error) {
		if (error instanceof DurableSessionLogError && error.code === "malformed_history" && code !== error.code) {
			fail(code, error.message);
		}
		throw error;
	}
	return normalized as DurableSessionEventPayload;
}

function eventBytes(event: DurableSessionEvent): number {
	return Buffer.byteLength(JSON.stringify(event), "utf8");
}

function commandFingerprint(binding: SessionBinding, commandId: string, text: string): string {
	return sha256(canonicalJson({
		kind: "nopal.session.command-fingerprint/v1",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		command_id: commandId,
		text,
	}, "command fingerprint"));
}

function cursorMaterial(
	binding: SessionBinding,
	streamId: string,
	sequence: number,
	previousCursor: string | null,
	eventId: string,
	commandId: string | undefined,
	event: DurableSessionEventPayload,
): string {
	return canonicalJson({
		kind: "nopal.session.cursor-material/v1",
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		stream_id: streamId,
		sequence,
		previous_cursor: previousCursor,
		event_id: eventId,
		command_id: commandId ?? null,
		event,
	}, "cursor material");
}

function createCursor(
	binding: SessionBinding,
	streamId: string,
	sequence: number,
	previousCursor: string | null,
	eventId: string,
	commandId: string | undefined,
	event: DurableSessionEventPayload,
): string {
	const streamDigest = streamId.slice(SESSION_STREAM_PREFIX.length);
	return `${SESSION_CURSOR_PREFIX}${streamDigest}:${sequence}:${sha256(cursorMaterial(
		binding,
		streamId,
		sequence,
		previousCursor,
		eventId,
		commandId,
		event,
	))}`;
}

function rootExtras(value: Record<string, unknown>): Record<string, JsonValue> {
	const reserved = new Set([
		"kind",
		"event_id",
		"plot_id",
		"session_id",
		"stream_id",
		"sequence",
		"previous_cursor",
		"cursor",
		"command_id",
		"event",
	]);
	const extras: Record<string, JsonValue> = {};
	for (const key of Object.keys(value).sort()) {
		if (!reserved.has(key)) {
			defineJsonField(extras, key, normalizeJson(value[key], `event envelope.${key}`));
		}
	}
	return extras;
}

function normalizeHistoryEvent(
	value: unknown,
	binding: SessionBinding,
	streamId: string,
	sequence: number,
	previousCursor: string | null,
	appendKind: DurableSessionEventKind,
): DurableSessionEvent {
	if (!isObject(value)) fail("malformed_history", "durable Session custom entry data is not an object");
	if (
		value.kind !== SESSION_EVENT_V1_KIND
		&& value.kind !== SESSION_EVENT_V2_KIND
		&& !(appendKind === SESSION_EVENT_V3_KIND && value.kind === SESSION_EVENT_V3_KIND)
	) {
		fail("malformed_history", `unsupported durable Session event kind ${JSON.stringify(value.kind)}`);
	}
	const eventKind = value.kind === SESSION_EVENT_V1_KIND
		? SESSION_EVENT_V2_KIND
		: value.kind as DurableSessionEventKind;
	const eventId = requireIdentity(value.event_id, "event_id", "malformed_history");
	const plotId = requireIdentity(value.plot_id, "plot_id", "malformed_history");
	const sessionId = requireIdentity(value.session_id, "session_id", "malformed_history");
	if (plotId !== binding.plotId || sessionId !== binding.sessionId) {
		fail("foreign_history", `durable history belongs to ${plotId}/${sessionId}, expected ${binding.plotId}/${binding.sessionId}`);
	}
	const commandId = value.command_id === undefined
		? undefined
		: requireIdentity(value.command_id, "command_id", "malformed_history");
	const event = normalizePayload(value.event, eventKind, "malformed_history");
	const cursor = createCursor(binding, streamId, sequence, previousCursor, eventId, commandId, event);

	if (value.kind === SESSION_EVENT_V2_KIND || value.kind === SESSION_EVENT_V3_KIND) {
		if (value.stream_id !== streamId) fail("foreign_history", "durable Session stream identity does not match this Plot Session");
		if (value.sequence !== sequence || value.previous_cursor !== previousCursor || value.cursor !== cursor) {
			fail("history_corrupt", `durable Session cursor chain is corrupt at event ${JSON.stringify(eventId)}`);
		}
	}

	return deepFreeze({
		...rootExtras(value),
		kind: eventKind,
		event_id: eventId,
		plot_id: binding.plotId,
		session_id: binding.sessionId,
		stream_id: streamId,
		sequence,
		previous_cursor: previousCursor,
		cursor,
		...(commandId === undefined ? {} : { command_id: commandId }),
		event,
	}) as DurableSessionEvent;
}

/**
 * The authoritative structured journal for one exact Core Plot/Session binding.
 *
 * Hydration accepts only Nopal custom entries on Pi's active branch. Appends call
 * the persistence effect before changing any in-memory index, so a returned
 * event is the bridge's safe point for broadcast. Construct a replacement log
 * after Pi tree navigation rather than mutating one across branches.
 */
export class DurableSessionLog {
	readonly binding: Readonly<SessionBinding>;
	readonly streamId: string;
	readonly #streamDigest: string;
	readonly #appendEntry: DurableSessionLogOptions["appendEntry"];
	readonly #appendKind: DurableSessionEventKind;
	readonly #limits: ResolvedLimits;
	readonly #events: DurableSessionEvent[] = [];
	readonly #eventById = new Map<string, DurableSessionEvent>();
	readonly #eventIndexByCursor = new Map<string, number>();
	readonly #successorByPrevious = new Map<string, string>();
	readonly #commands = new Map<string, SeenCommand>();
	readonly #abandonedCursors = new Set<string>();
	#ready?: DurableSessionEvent;
	#bytes = 0;

	static hydrate(options: DurableSessionLogOptions): DurableSessionLog {
		return new DurableSessionLog(options);
	}

	constructor(options: DurableSessionLogOptions) {
		const plotId = requireIdentity(options.binding?.plotId, "plot_id", "invalid_binding");
		const sessionId = requireIdentity(options.binding?.sessionId, "session_id", "invalid_binding");
		if (!Array.isArray(options.activeBranch)) fail("malformed_history", "active Pi Session branch is not an array");
		if (typeof options.appendEntry !== "function") fail("invalid_binding", "appendEntry persistence callback is required");

		this.binding = deepFreeze({ plotId, sessionId });
		this.#streamDigest = sha256(canonicalJson({
			kind: "nopal.session.stream-binding/v1",
			plot_id: plotId,
			session_id: sessionId,
		}, "stream binding"));
		this.streamId = `${SESSION_STREAM_PREFIX}${this.#streamDigest}`;
		this.#appendEntry = options.appendEntry;
		this.#appendKind = resolveAppendKind(options.appendKind);
		this.#limits = resolveLimits(options.limits);

		for (const entry of options.activeBranch) {
			if (!isObject(entry)) fail("malformed_history", "active Pi Session branch contains a malformed entry");
			if (entry.customType !== SESSION_EVENT_ENTRY) continue;
			if (entry.type !== "custom") fail("malformed_history", "nopal-session-event is not a Pi custom entry");
			const sequence = this.#events.length + 1;
			const event = normalizeHistoryEvent(
				entry.data,
				this.binding,
				this.streamId,
				sequence,
				this.headCursor,
				this.#appendKind,
			);
			const bytes = eventBytes(event);
			this.#assertCanIndex(event, bytes);
			this.#commitIndex(event, bytes);
		}
		this.#registerAbandonedCursors(options.abandonedCursors);
	}

	get eventCount(): number {
		return this.#events.length;
	}

	get byteCount(): number {
		return this.#bytes;
	}

	get headCursor(): string | null {
		return this.#events.at(-1)?.cursor ?? null;
	}

	get headSequence(): number {
		return this.#events.length;
	}

	events(): readonly DurableSessionEvent[] {
		return Object.freeze([...this.#events]);
	}

	ready(): DurableSessionEvent | undefined {
		return this.#ready;
	}

	eventById(eventId: string): DurableSessionEvent | undefined {
		return this.#eventById.get(eventId);
	}

	eventByCursor(cursor: string): DurableSessionEvent | undefined {
		const index = this.#eventIndexByCursor.get(cursor);
		return index === undefined ? undefined : this.#events[index];
	}

	successorOf(previousCursor: string | null): DurableSessionEvent | undefined {
		const cursor = this.#successorByPrevious.get(previousCursor ?? GENESIS);
		return cursor === undefined ? undefined : this.eventByCursor(cursor);
	}

	ensureReady(eventId: string): DurableSessionEvent {
		return this.#ready ?? this.append({ eventId, event: { type: "session_ready" } });
	}

	/** Check a prompt before appending its user event or delivering it to Pi. */
	classifyCommand(commandId: string, text: string): CommandDisposition {
		const validCommandId = requireIdentity(commandId, "command_id", "malformed_history");
		if (typeof text !== "string" || text.trim().length === 0) {
			fail("malformed_history", "durable Session command text is empty");
		}
		const fingerprint = commandFingerprint(this.binding, validCommandId, text);
		const seen = this.#commands.get(validCommandId);
		if (!seen) return { kind: "new", fingerprint };
		if (seen.fingerprint !== fingerprint) {
			fail("command_conflict", `command_id ${JSON.stringify(validCommandId)} was already committed with different content`);
		}
		return { kind: "duplicate", event: seen.event };
	}

	append(input: AppendSessionEvent): DurableSessionEvent {
		const eventId = requireIdentity(input.eventId, "event_id", "malformed_history");
		const commandId = input.commandId === undefined
			? undefined
			: requireIdentity(input.commandId, "command_id", "malformed_history");
		const event = normalizePayload(input.event, this.#appendKind, "malformed_history");
		const sequence = this.#events.length + 1;
		const previousCursor = this.headCursor;
		const cursor = createCursor(this.binding, this.streamId, sequence, previousCursor, eventId, commandId, event);
		const extra = input.extra === undefined ? {} : rootExtras(input.extra);
		const envelope = deepFreeze({
			...extra,
			kind: this.#appendKind,
			event_id: eventId,
			plot_id: this.binding.plotId,
			session_id: this.binding.sessionId,
			stream_id: this.streamId,
			sequence,
			previous_cursor: previousCursor,
			cursor,
			...(commandId === undefined ? {} : { command_id: commandId }),
			event,
		}) as DurableSessionEvent;
		const bytes = eventBytes(envelope);
		this.#assertCanIndex(envelope, bytes);

		try {
			this.#appendEntry(SESSION_EVENT_ENTRY, envelope);
		} catch (error) {
			throw new DurableSessionLogError(
				"persistence_failed",
				`could not persist durable Session event ${JSON.stringify(eventId)}: ${error instanceof Error ? error.message : String(error)}`,
				{ cause: error },
			);
		}
		this.#commitIndex(envelope, bytes);
		return envelope;
	}

	/**
	 * Return one immutable active-branch page after a verified cursor.
	 * Null means genesis. Typed errors distinguish malformed, foreign-stream, and
	 * same-stream unknown or abandoned cursors so the bridge never silently skips
	 * a gap or mistakes fabricated history for an abandoned branch.
	 */
	eventsAfter(afterCursor: string | null, limit = this.#limits.maxReplayPageEvents): SessionReplaySlice {
		if (!Number.isSafeInteger(limit) || limit <= 0 || limit > this.#limits.maxReplayPageEvents) {
			fail("invalid_limit", `replay limit must be between 1 and ${this.#limits.maxReplayPageEvents}`);
		}
		let start = 0;
		if (afterCursor !== null) {
			const match = CURSOR_PATTERN.exec(afterCursor);
			if (!match) fail("malformed_cursor", "Session resume cursor is malformed");
			if (match[1] !== this.#streamDigest) fail("foreign_cursor", "Session resume cursor belongs to another Plot Session stream");
			const index = this.#eventIndexByCursor.get(afterCursor);
			if (index === undefined) {
				if (this.#abandonedCursors.has(afterCursor)) {
					fail("branch_divergence", "Session resume cursor belongs to an abandoned Pi branch suffix");
				}
				fail("history_gap", "Session resume cursor is unknown to this Pi Session history");
			}
			start = index + 1;
		}
		const page = Object.freeze(this.#events.slice(start, start + limit));
		const nextCursor = page.at(-1)?.cursor ?? afterCursor;
		return {
			fromCursor: afterCursor,
			events: page,
			nextCursor,
			headCursor: this.headCursor,
			hasMore: start + page.length < this.#events.length,
		};
	}

	#assertCanIndex(event: DurableSessionEvent, bytes: number): void {
		if (bytes > this.#limits.maxEventBytes) {
			fail("event_too_large", `durable Session event is ${bytes} bytes; limit is ${this.#limits.maxEventBytes}`);
		}
		if (this.#events.length >= this.#limits.maxEvents || this.#bytes + bytes > this.#limits.maxBytes) {
			fail("history_too_large", "durable Session history exceeds its configured bound");
		}
		if (this.#eventById.has(event.event_id)) {
			fail("duplicate_event", `durable Session event_id ${JSON.stringify(event.event_id)} appears more than once`);
		}
		if (this.#eventIndexByCursor.has(event.cursor)) {
			fail("duplicate_event", `durable Session cursor ${JSON.stringify(event.cursor)} appears more than once`);
		}
		if (event.previous_cursor !== this.headCursor) {
			fail("history_corrupt", `event ${JSON.stringify(event.event_id)} does not extend the active durable head`);
		}
		const predecessorKey = event.previous_cursor ?? GENESIS;
		if (this.#successorByPrevious.has(predecessorKey)) {
			fail("history_corrupt", "durable Session predecessor already has a different successor");
		}
		if (event.event.type === "user_message") {
			if (event.command_id === undefined) {
				fail("history_corrupt", "durable Session user_message requires command_id");
			}
			const text = event.event.text;
			if (typeof text !== "string") fail("malformed_history", "structured user event has no command text");
			const fingerprint = commandFingerprint(this.binding, event.command_id, text);
			const seen = this.#commands.get(event.command_id);
			if (seen?.fingerprint === fingerprint) {
				fail("duplicate_command_event", `command_id ${JSON.stringify(event.command_id)} has multiple durable user events`);
			}
			if (seen) fail("command_conflict", `command_id ${JSON.stringify(event.command_id)} has conflicting durable content`);
		}
		if (
			(event.event.type === "assistant_message" || event.event.type === "session_error")
			&& event.command_id !== undefined
			&& !this.#commands.has(event.command_id)
		) {
			fail(
				"history_corrupt",
				`durable Session ${event.event.type} references unknown command_id ${JSON.stringify(event.command_id)}`,
			);
		}
		if (
			event.kind === SESSION_EVENT_V3_KIND
			&& isV3ActivityEventType(event.event.type)
			&& event.command_id !== undefined
			&& !this.#commands.has(event.command_id)
		) {
			fail(
				"history_corrupt",
				`durable Session ${event.event.type} references unknown command_id ${JSON.stringify(event.command_id)}`,
			);
		}
	}

	#commitIndex(event: DurableSessionEvent, bytes: number): void {
		const index = this.#events.length;
		this.#events.push(event);
		this.#eventById.set(event.event_id, event);
		this.#eventIndexByCursor.set(event.cursor, index);
		this.#abandonedCursors.delete(event.cursor);
		this.#successorByPrevious.set(event.previous_cursor ?? GENESIS, event.cursor);
		this.#bytes += bytes;
		if (this.#ready === undefined && event.event.type === "session_ready") this.#ready = event;
		if (event.event.type === "user_message" && event.command_id !== undefined) {
			const text = event.event.text;
			if (typeof text === "string") {
				this.#commands.set(event.command_id, {
					fingerprint: commandFingerprint(this.binding, event.command_id, text),
					event,
				});
			}
		}
	}

	#registerAbandonedCursors(cursors: readonly string[] | undefined): void {
		if (cursors === undefined) return;
		if (!Array.isArray(cursors)) fail("history_corrupt", "abandoned Session cursor registry is not an array");
		for (const cursor of cursors) {
			const match = typeof cursor === "string" ? CURSOR_PATTERN.exec(cursor) : null;
			if (!match) fail("history_corrupt", "abandoned Session cursor registry contains a malformed cursor");
			if (match[1] !== this.#streamDigest) {
				fail("foreign_history", "abandoned Session cursor registry belongs to another Plot Session stream");
			}
			if (this.#eventIndexByCursor.has(cursor)) {
				fail("history_corrupt", "one Session cursor is marked both active and abandoned");
			}
			this.#abandonedCursors.add(cursor);
		}
	}
}
