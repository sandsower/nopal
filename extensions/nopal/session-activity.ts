import { createHash } from "node:crypto";

import type {
	ExtensionAPI,
	ExtensionContext,
	ToolCallEvent,
	ToolResultEvent,
} from "@earendil-works/pi-coding-agent";

import {
	MAX_ACTIVITY_DISPLAY_BYTES,
	MAX_ACTIVITY_FAILURE_BYTES,
	MAX_ACTIVITY_OUTPUT_BYTES,
	MAX_ACTIVITY_TOOL_NAME_BYTES,
	MAX_SESSION_IDENTITY_BYTES,
	type AppendSessionEvent,
	type DurableSessionEvent,
	type DurableSessionEventPayload,
	type SessionBinding,
} from "./session-log.js";

const ACTIVITY_ID_PREFIX = "nopal.session.activity/v1:";
const ACTIVITY_EVENT_ID_PREFIX = "nopal.session.activity-event/v1:";
const UNAVAILABLE_EXIT_REASON = "Pi tool_result does not expose an exit code or signal";
const TRUNCATION_MARKER = "\n...[truncated]...\n";

type ActivityKind = "command" | "tool";
type ActivityTerminalType = "command_finished" | "command_failed" | "tool_finished" | "tool_failed";

type ActivityState = {
	activityId: string;
	toolCallId: string;
	observedToolName: string;
	persistedToolName: string;
	kind: ActivityKind;
	commandId?: string;
	startFingerprint: string;
	startedMonotonic?: number;
	terminalType?: ActivityTerminalType;
	terminalFingerprint?: string;
};

export type ActivityProductionErrorCode =
	| "invalid_identity"
	| "invalid_clock"
	| "identity_conflict"
	| "orphan_terminal"
	| "terminal_conflict"
	| "duration_unavailable"
	| "history_conflict";

export class ActivityProductionError extends Error {
	readonly code: ActivityProductionErrorCode;

	constructor(code: ActivityProductionErrorCode, message: string) {
		super(message);
		this.name = "ActivityProductionError";
		this.code = code;
	}
}

export type BoundedActivityText = {
	text: string;
	truncated: boolean;
	original_bytes: number;
	omitted_bytes: number;
	details_unavailable?: boolean;
};

export type SessionActivityProducerOptions = {
	binding: SessionBinding;
	existingEvents?: readonly DurableSessionEvent[];
	publish(input: AppendSessionEvent): unknown;
	monotonicNow?: () => number;
	wallNow?: () => string;
};

export type SessionActivityHookOptions = {
	producer(): SessionActivityProducer | undefined;
	commandId?(): string | undefined;
	onError?(error: unknown, context: ExtensionContext): void;
};

function sha256(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function byteLength(value: string): number {
	return Buffer.byteLength(value, "utf8");
}

function isSafeIdentity(value: unknown): value is string {
	return typeof value === "string"
		&& value.trim().length > 0
		&& byteLength(value) <= MAX_SESSION_IDENTITY_BYTES
		&& !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function requireIdentity(value: unknown, field: string): string {
	if (!isSafeIdentity(value)) {
		throw new ActivityProductionError("invalid_identity", `invalid Pi activity ${field}`);
	}
	return value;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? value as Record<string, unknown>
		: undefined;
}

function textField(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function title(toolName: string): string {
	const [first, ...rest] = [...toolName];
	return first === undefined ? "Tool" : `${first.toUpperCase()}${rest.join("")}`;
}

/**
 * Redact only recognized credential presentations. Unknown payloads are never
 * serialized, so this is a final defense for allowlisted display strings.
 */
export function redactActivityText(value: string): string {
	return value
		.replace(/\b(Bearer\s+)[A-Za-z0-9._~+/=-]+/giu, "$1[REDACTED]")
		.replace(/\b(gh[opusr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}|sk-[A-Za-z0-9_-]{16,})\b/gu, "[REDACTED]")
		.replace(
			/(\b(?:api[_-]?(?:key|token)|access[_-]?token|auth(?:orization)?|client[_-]?secret|password|passwd|secret|token)\b\s*[=:]\s*)(["']?)[^\s,"']+\2/giu,
			"$1[REDACTED]",
		)
		.replace(
			/(--(?:api[_-]?key|access[_-]?token|auth(?:orization)?|client[_-]?secret|password|secret|token)(?:=|\s+))[^\s]+/giu,
			"$1[REDACTED]",
		)
		.replace(/(https?:\/\/)[^\s/@:]+:[^\s/@]+@/giu, "$1[REDACTED]@");
}

function prefixWithinBytes(value: string, maximumBytes: number): string {
	let result = "";
	let bytes = 0;
	for (const character of value) {
		const next = byteLength(character);
		if (bytes + next > maximumBytes) break;
		result += character;
		bytes += next;
	}
	return result;
}

function suffixWithinBytes(value: string, maximumBytes: number): string {
	const characters = [...value];
	let result = "";
	let bytes = 0;
	for (let index = characters.length - 1; index >= 0; index -= 1) {
		const character = characters[index] ?? "";
		const next = byteLength(character);
		if (bytes + next > maximumBytes) break;
		result = `${character}${result}`;
		bytes += next;
	}
	return result;
}

/** Bound a redacted display value without splitting a UTF-8 scalar. */
export function boundActivityText(value: string, maximumBytes: number): BoundedActivityText {
	if (!Number.isSafeInteger(maximumBytes) || maximumBytes < byteLength(TRUNCATION_MARKER)) {
		throw new RangeError("activity text maximum must fit the truncation marker");
	}
	const redacted = redactActivityText(value);
	const originalBytes = byteLength(redacted);
	if (originalBytes <= maximumBytes) {
		return { text: redacted, truncated: false, original_bytes: originalBytes, omitted_bytes: 0 };
	}
	const remaining = maximumBytes - byteLength(TRUNCATION_MARKER);
	const head = prefixWithinBytes(redacted, Math.ceil(remaining / 2));
	const tail = suffixWithinBytes(redacted, Math.floor(remaining / 2));
	const text = `${head}${TRUNCATION_MARKER}${tail}`;
	const retainedBytes = byteLength(text);
	return {
		text,
		truncated: true,
		original_bytes: originalBytes,
		omitted_bytes: originalBytes - retainedBytes,
	};
}

function boundedDisplay(value: string, maximumBytes: number): string {
	const withoutControls = value.replace(/[\u0000-\u001f\u007f-\u009f]/gu, (character) => {
		switch (character) {
			case "\n": return "\\n";
			case "\r": return "\\r";
			case "\t": return "\\t";
			default: return `\\u{${character.codePointAt(0)?.toString(16).padStart(4, "0")}}`;
		}
	});
	return boundActivityText(withoutControls, maximumBytes).text;
}

function activityId(binding: SessionBinding, toolCallId: string): string {
	return `${ACTIVITY_ID_PREFIX}${sha256(`${binding.plotId}\0${binding.sessionId}\0${toolCallId}`)}`;
}

function eventId(activity: string, phase: "start" | "finish" | "failure"): string {
	return `${ACTIVITY_EVENT_ID_PREFIX}${sha256(`${activity}\0${phase}`)}`;
}

function startFingerprint(kind: ActivityKind, toolName: string, presentation: unknown): string {
	return sha256(canonicalJson({ kind, toolName, presentation }));
}

function canonicalJson(value: unknown): string {
	if (value === null || typeof value !== "object") return JSON.stringify(value);
	if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
	const record = value as Record<string, unknown>;
	return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(",")}}`;
}

function terminalObservation(payload: Record<string, unknown>): Record<string, unknown> {
	const { duration_ms: _duration, ...observation } = payload;
	return observation;
}

function requireToolName(value: unknown): string {
	const toolName = requireIdentity(value, "tool_name");
	if (byteLength(toolName) > MAX_ACTIVITY_TOOL_NAME_BYTES) {
		throw new ActivityProductionError("invalid_identity", "invalid Pi activity tool_name");
	}
	return toolName;
}

function knownToolSummary(toolName: string, input: unknown): BoundedActivityText {
	const record = asRecord(input) ?? {};
	let summary: string;
	switch (toolName) {
		case "read":
			summary = `Read ${textField(record.path) ?? "path unavailable"}`;
			break;
		case "edit": {
			const count = Array.isArray(record.edits) ? record.edits.length : undefined;
			summary = `Edit ${textField(record.path) ?? "path unavailable"}${count === undefined ? "" : ` (${count} edit${count === 1 ? "" : "s"})`}`;
			break;
		}
		case "write": {
			const content = textField(record.content);
			summary = `Write ${textField(record.path) ?? "path unavailable"}${content === undefined ? "" : ` (${byteLength(content)} bytes)`}`;
			break;
		}
		case "grep":
			summary = `Search ${textField(record.pattern) ?? "pattern unavailable"} in ${textField(record.path) ?? textField(record.glob) ?? "workspace"}`;
			break;
		case "find":
			summary = `Find ${textField(record.pattern) ?? "pattern unavailable"} in ${textField(record.path) ?? "workspace"}`;
			break;
		case "ls":
			summary = `List ${textField(record.path) ?? "workspace"}`;
			break;
		default:
			return {
				...boundActivityText("Details unavailable", MAX_ACTIVITY_DISPLAY_BYTES),
				details_unavailable: true,
			};
	}
	return boundActivityText(summary, MAX_ACTIVITY_DISPLAY_BYTES);
}

function resultSummary(toolName: string): BoundedActivityText {
	if (!["read", "edit", "write", "grep", "find", "ls"].includes(toolName)) {
		return {
			...boundActivityText("Details unavailable", MAX_ACTIVITY_DISPLAY_BYTES),
			details_unavailable: true,
		};
	}
	return boundActivityText(`${title(toolName)} completed`, MAX_ACTIVITY_DISPLAY_BYTES);
}

class BoundedTextAccumulator {
	readonly #maximumBytes: number;
	readonly #headBytes: number;
	readonly #tailBytes: number;
	#head = "";
	#tail = "";
	#complete = "";
	#originalBytes = 0;
	#hasText = false;

	constructor(maximumBytes: number) {
		this.#maximumBytes = maximumBytes;
		const remaining = maximumBytes - byteLength(TRUNCATION_MARKER);
		this.#headBytes = Math.ceil(remaining / 2);
		this.#tailBytes = Math.floor(remaining / 2);
	}

	addRedacted(value: string): void {
		if (value.length === 0) return;
		const bytes = byteLength(value);
		if (bytes === 0) return;
		this.#hasText = true;
		this.#originalBytes += bytes;
		if (byteLength(this.#head) < this.#headBytes) {
			this.#head += prefixWithinBytes(value, this.#headBytes - byteLength(this.#head));
		}
		this.#tail = bytes >= this.#tailBytes
			? suffixWithinBytes(value, this.#tailBytes)
			: suffixWithinBytes(`${this.#tail}${value}`, this.#tailBytes);
		this.#complete = this.#originalBytes <= this.#maximumBytes
			? `${this.#complete}${value}`
			: "";
	}

	finish(): BoundedActivityText | undefined {
		if (!this.#hasText) return undefined;
		if (this.#originalBytes <= this.#maximumBytes) {
			return {
				text: this.#complete,
				truncated: false,
				original_bytes: this.#originalBytes,
				omitted_bytes: 0,
			};
		}
		const text = `${this.#head}${TRUNCATION_MARKER}${this.#tail}`;
		return {
			text,
			truncated: true,
			original_bytes: this.#originalBytes,
			omitted_bytes: this.#originalBytes - byteLength(text),
		};
	}
}

const STREAM_REDACTION_CHUNK_UNITS = 16 * 1024;
const STREAM_REDACTION_CARRY_UNITS = 512;
const STREAM_REDACTION_MARKER_OVERLAP = 128;

/**
 * Delay a bounded suffix across Pi content parts so credential presentations
 * split at any part boundary are redacted as one logical text stream. Large
 * parts are scanned in fixed windows and never joined into one aggregate copy.
 */
class StreamingActivityRedactor {
	readonly #emit: (redacted: string) => void;
	#pending = "";
	#discardSensitiveToken = false;

	constructor(emit: (redacted: string) => void) {
		this.#emit = emit;
	}

	add(value: string): void {
		let start = 0;
		while (start < value.length) {
			let end = Math.min(value.length, start + STREAM_REDACTION_CHUNK_UNITS);
			if (end < value.length && /[\uD800-\uDBFF]/u.test(value[end - 1] ?? "")) end -= 1;
			this.#pending += value.slice(start, end);
			start = end;
			this.#drain(false);
		}
	}

	finish(): void {
		this.#drain(true);
	}

	#drain(final: boolean): void {
		while (this.#pending.length > 0) {
			if (this.#discardSensitiveToken) {
				const delimiter = this.#pending.search(/[\s,"']/u);
				if (delimiter < 0) {
					this.#pending = "";
					return;
				}
				this.#pending = this.#pending.slice(delimiter);
				this.#discardSensitiveToken = false;
			}
			if (final) {
				this.#emit(redactActivityText(this.#pending));
				this.#pending = "";
				return;
			}
			if (this.#pending.length <= STREAM_REDACTION_CHUNK_UNITS + STREAM_REDACTION_CARRY_UNITS) {
				return;
			}
			const target = this.#pending.length - STREAM_REDACTION_CARRY_UNITS;
			let cut = -1;
			for (let index = target; index >= 0; index -= 1) {
				if (/[\s,"']/u.test(this.#pending[index] ?? "")) {
					cut = index + 1;
					break;
				}
			}
			if (cut > 0) {
				const candidate = this.#pending.slice(0, cut);
				const openCredential = /(?:\bBearer\s+|--(?:api[_-]?key|access[_-]?token|authorization|client[_-]?secret|password|secret|token)(?:=|\s+)|\b(?:api[_-]?(?:key|token)|access[_-]?token|authorization|client[_-]?secret|password|passwd|secret|token)\b\s*[=:]\s*)$/iu.exec(candidate);
				if (openCredential && openCredential.index > 0) {
					cut = openCredential.index;
				}
				if (cut > 0) {
					this.#emit(redactActivityText(this.#pending.slice(0, cut)));
					this.#pending = this.#pending.slice(cut);
					continue;
				}
			}
			const redacted = redactActivityText(this.#pending);
			if (redacted !== this.#pending) {
				this.#emit(redacted);
				this.#pending = "";
				this.#discardSensitiveToken = true;
				return;
			}
			let rawCut = this.#pending.length - STREAM_REDACTION_MARKER_OVERLAP;
			if (/[\uD800-\uDBFF]/u.test(this.#pending[rawCut - 1] ?? "")) rawCut -= 1;
			this.#emit(this.#pending.slice(0, rawCut));
			this.#pending = this.#pending.slice(rawCut);
		}
	}
}

function observedTextContent(event: ToolResultEvent, maximumBytes: number): BoundedActivityText | undefined {
	const accumulator = new BoundedTextAccumulator(maximumBytes);
	const redactor = new StreamingActivityRedactor((redacted) => accumulator.addRedacted(redacted));
	for (const part of event.content) {
		const record = asRecord(part);
		if (record?.type !== "text" || typeof record.text !== "string") continue;
		redactor.add(record.text);
	}
	redactor.finish();
	return accumulator.finish();
}

function eventType(value: DurableSessionEventPayload): string {
	return value.type;
}

function activityEventRecord(event: DurableSessionEvent): Record<string, unknown> | undefined {
	const type = event.event.type;
	return type === "command_started"
		|| type === "command_finished"
		|| type === "command_failed"
		|| type === "tool_started"
		|| type === "tool_finished"
		|| type === "tool_failed"
		? event.event as Record<string, unknown>
		: undefined;
}

export class SessionActivityProducer {
	readonly #binding: SessionBinding;
	readonly #publish: SessionActivityProducerOptions["publish"];
	readonly #monotonicNow: () => number;
	readonly #wallNow: () => string;
	readonly #activities = new Map<string, ActivityState>();

	constructor(options: SessionActivityProducerOptions) {
		this.#binding = {
			plotId: requireIdentity(options.binding.plotId, "plot_id"),
			sessionId: requireIdentity(options.binding.sessionId, "session_id"),
		};
		this.#publish = options.publish;
		this.#monotonicNow = options.monotonicNow ?? (() => performance.now());
		this.#wallNow = options.wallNow ?? (() => new Date().toISOString());
		this.#hydrate(options.existingEvents ?? []);
	}

	observeToolCall(event: ToolCallEvent, commandId?: string): void {
		const toolCallId = requireIdentity(event.toolCallId, "tool_call_id");
		const observedToolName = requireToolName(event.toolName);
		const safeCommandId = commandId === undefined ? undefined : requireIdentity(commandId, "command_id");
		const kind: ActivityKind = observedToolName === "bash" ? "command" : "tool";
		const persistedToolName = redactActivityText(observedToolName);
		const activity = activityId(this.#binding, toolCallId);
		const command = kind === "command"
			? boundedDisplay(textField(asRecord(event.input)?.command) ?? "Command unavailable", MAX_ACTIVITY_DISPLAY_BYTES)
			: undefined;
		const summary = kind === "tool" ? knownToolSummary(observedToolName, event.input) : undefined;
		const fingerprint = startFingerprint(kind, observedToolName, command ?? summary);
		const existing = this.#activities.get(toolCallId);
		if (existing) {
			if (
				existing.kind !== kind
				|| existing.observedToolName !== observedToolName
				|| existing.commandId !== safeCommandId
				|| existing.startFingerprint !== fingerprint
			) {
				throw new ActivityProductionError("identity_conflict", `Pi toolCallId ${JSON.stringify(toolCallId)} was reused with conflicting activity facts`);
			}
			return;
		}
		const startedMonotonic = this.#readMonotonic();
		const startedAt = this.#readWall();
		const payload = kind === "command"
			? {
				type: "command_started",
				activity_id: activity,
				tool_call_id: toolCallId,
				command,
				started_at: startedAt,
			}
			: {
				type: "tool_started",
				activity_id: activity,
				tool_call_id: toolCallId,
				tool_name: persistedToolName,
				summary,
				started_at: startedAt,
			};
		this.#publish({ eventId: eventId(activity, "start"), commandId: safeCommandId, event: payload });
		this.#activities.set(toolCallId, {
			activityId: activity,
			toolCallId,
			observedToolName,
			persistedToolName,
			kind,
			commandId: safeCommandId,
			startFingerprint: fingerprint,
			startedMonotonic,
		});
	}

	observeToolResult(event: ToolResultEvent, commandId?: string): void {
		const toolCallId = requireIdentity(event.toolCallId, "tool_call_id");
		const observedToolName = requireToolName(event.toolName);
		const state = this.#activities.get(toolCallId);
		if (!state) {
			throw new ActivityProductionError("orphan_terminal", `Pi tool result ${JSON.stringify(toolCallId)} has no observed start`);
		}
		const suppliedCommandId = commandId === undefined ? undefined : requireIdentity(commandId, "command_id");
		if (
			state.observedToolName !== observedToolName
			|| (suppliedCommandId !== undefined && suppliedCommandId !== state.commandId)
		) {
			throw new ActivityProductionError("identity_conflict", `Pi tool result ${JSON.stringify(toolCallId)} conflicts with its observed start`);
		}
		const terminalType: ActivityTerminalType = state.kind === "command"
			? event.isError ? "command_failed" : "command_finished"
			: event.isError ? "tool_failed" : "tool_finished";
		if (!state.terminalType && state.startedMonotonic === undefined) {
			throw new ActivityProductionError("duration_unavailable", `Pi tool result ${JSON.stringify(toolCallId)} resumed without a local monotonic start`);
		}
		const duration = state.startedMonotonic === undefined
			? 0
			: this.#readMonotonic() - state.startedMonotonic;
		if (!Number.isSafeInteger(Math.floor(duration)) || duration < 0) {
			throw new ActivityProductionError("invalid_clock", "Pi activity monotonic clock moved backwards");
		}
		const durationMs = Math.floor(duration);
		let payload: Record<string, unknown>;
		if (terminalType === "command_finished") {
			const output = observedTextContent(event, MAX_ACTIVITY_OUTPUT_BYTES);
			payload = {
				type: terminalType,
				activity_id: state.activityId,
				tool_call_id: toolCallId,
				duration_ms: durationMs,
				exit: { type: "unavailable", reason: UNAVAILABLE_EXIT_REASON },
				outcome: "succeeded",
				...(output === undefined
					? {}
					: { output: { channel: "combined", ...output } }),
			};
		} else if (terminalType === "command_failed") {
			const failure = observedTextContent(event, MAX_ACTIVITY_FAILURE_BYTES);
			payload = {
				type: terminalType,
				activity_id: state.activityId,
				tool_call_id: toolCallId,
				duration_ms: durationMs,
				message: boundedDisplay(failure?.text ?? "Shell command failed", MAX_ACTIVITY_FAILURE_BYTES),
			};
		} else if (terminalType === "tool_finished") {
			payload = {
				type: terminalType,
				activity_id: state.activityId,
				tool_call_id: toolCallId,
				duration_ms: durationMs,
				outcome: "succeeded",
				summary: resultSummary(state.observedToolName),
			};
		} else {
			payload = {
				type: terminalType,
				activity_id: state.activityId,
				tool_call_id: toolCallId,
				duration_ms: durationMs,
				message: boundedDisplay(`${title(state.persistedToolName)} failed`, MAX_ACTIVITY_FAILURE_BYTES),
				outcome: "failed",
			};
		}
		const terminalFingerprint = sha256(canonicalJson(terminalObservation(payload)));
		if (state.terminalType) {
			const duplicate = state.terminalFingerprint === terminalFingerprint;
			if (state.terminalType !== terminalType || !duplicate) {
				throw new ActivityProductionError("terminal_conflict", `Pi tool result ${JSON.stringify(toolCallId)} conflicts with its durable terminal outcome`);
			}
			return;
		}
		try {
			this.#publish({
				eventId: eventId(state.activityId, event.isError ? "failure" : "finish"),
				commandId: state.commandId,
				event: payload,
			});
		} catch (error) {
			this.#activities.delete(toolCallId);
			throw error;
		}
		state.terminalType = terminalType;
		state.terminalFingerprint = terminalFingerprint;
	}

	#readMonotonic(): number {
		const value = this.#monotonicNow();
		if (!Number.isFinite(value) || value < 0) {
			throw new ActivityProductionError("invalid_clock", "Pi activity monotonic clock returned an invalid value");
		}
		return value;
	}

	#readWall(): string {
		const value = this.#wallNow();
		if (
			typeof value !== "string"
			|| !Number.isFinite(Date.parse(value))
			|| byteLength(value) > MAX_ACTIVITY_DISPLAY_BYTES
			|| /[\u0000-\u001f\u007f-\u009f]/u.test(value)
		) {
			throw new ActivityProductionError("invalid_clock", "Pi activity wall clock returned an invalid timestamp");
		}
		return value;
	}

	#hydrate(events: readonly DurableSessionEvent[]): void {
		for (const envelope of events) {
			const event = activityEventRecord(envelope);
			if (!event) continue;
			const toolCallId = requireIdentity(event.tool_call_id, "tool_call_id");
			const activity = requireIdentity(event.activity_id, "activity_id");
			if (activity !== activityId(this.#binding, toolCallId)) {
				throw new ActivityProductionError("history_conflict", `durable activity ${JSON.stringify(toolCallId)} has an unstable activity_id`);
			}
			const type = eventType(envelope.event);
			if (type === "command_started" || type === "tool_started") {
				if (this.#activities.has(toolCallId)) {
					throw new ActivityProductionError("history_conflict", `durable activity ${JSON.stringify(toolCallId)} has duplicate starts`);
				}
				const kind: ActivityKind = type === "command_started" ? "command" : "tool";
				const observedToolName = kind === "command" ? "bash" : requireToolName(event.tool_name);
				const presentation = kind === "command" ? event.command : event.summary;
				this.#activities.set(toolCallId, {
					activityId: activity,
					toolCallId,
					observedToolName,
					persistedToolName: observedToolName,
					kind,
					commandId: envelope.command_id,
					startFingerprint: startFingerprint(kind, observedToolName, presentation),
				});
				continue;
			}
			const state = this.#activities.get(toolCallId);
			if (!state) {
				throw new ActivityProductionError("history_conflict", `durable activity terminal ${JSON.stringify(toolCallId)} has no start`);
			}
			if (state.activityId !== activity || state.commandId !== envelope.command_id) {
				throw new ActivityProductionError("history_conflict", `durable activity terminal ${JSON.stringify(toolCallId)} conflicts with its start`);
			}
			const terminalType = type as ActivityTerminalType;
			if (state.terminalType) {
				throw new ActivityProductionError("history_conflict", `durable activity ${JSON.stringify(toolCallId)} has multiple terminal events`);
			}
			if ((state.kind === "command") !== terminalType.startsWith("command_")) {
				throw new ActivityProductionError("history_conflict", `durable activity ${JSON.stringify(toolCallId)} changes kind`);
			}
			state.terminalType = terminalType;
			state.terminalFingerprint = sha256(canonicalJson(terminalObservation(event)));
		}
	}
}

/** Register only Pi's documented mutable pre-call and observable result hooks. */
export function registerSessionActivityHooks(pi: ExtensionAPI, options: SessionActivityHookOptions): void {
	const observe = (
		context: ExtensionContext,
		action: (producer: SessionActivityProducer, commandId: string | undefined) => void,
	) => {
		try {
			const producer = options.producer();
			if (!producer) return;
			action(producer, options.commandId?.());
		} catch (error) {
			try {
				options.onError?.(error, context);
			} catch {
				// Diagnostics are best effort and must never block or mutate Pi's tool lifecycle.
			}
		}
	};
	pi.on("tool_call", (event, context) => {
		observe(context, (producer, commandId) => producer.observeToolCall(event, commandId));
	});
	pi.on("tool_result", (event, context) => {
		observe(context, (producer, commandId) => producer.observeToolResult(event, commandId));
	});
}
