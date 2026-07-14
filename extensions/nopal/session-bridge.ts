import { randomUUID, createHash } from "node:crypto";
import { chmod, lstat, mkdir, unlink } from "node:fs/promises";
import { createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { TextDecoder } from "node:util";

import type { ExtensionAPI, InputSource } from "@earendil-works/pi-coding-agent";

import { resolveNopalSessionBinding, type ExecFn } from "./nopal-cli.js";
import {
	DurableSessionLog,
	DurableSessionLogError,
	MAX_ACTIVITY_FAILURE_BYTES,
	MAX_SESSION_IDENTITY_BYTES,
	SESSION_EVENT_ENTRY,
	SESSION_EVENT_V3_KIND,
	type AppendSessionEvent,
	type DurableSessionEvent,
	type DurableSessionEventKind,
	type DurableSessionEventPayload,
	type PiSessionEntry,
} from "./session-log.js";
import {
	SessionActivityProducer,
	boundActivityText,
	registerSessionActivityHooks,
} from "./session-activity.js";

export { SESSION_EVENT_ENTRY };
export { MAX_SESSION_IDENTITY_BYTES };
export const SESSION_ENDPOINT_KIND = "nopal.session/v3" as const;
export const SESSION_COMMAND_KIND = "nopal.session.command/v1" as const;
export const SESSION_EVENT_KIND = SESSION_EVENT_V3_KIND;
export const SESSION_SUBSCRIBE_KIND = "nopal.session.subscribe/v1" as const;
export const SESSION_REPLAY_COMPLETE_KIND = "nopal.session.replay_complete/v1" as const;
export const SESSION_FEED_ERROR_KIND = "nopal.session.feed_error/v1" as const;
export const MAX_JSONL_LINE_BYTES = 1024 * 1024;
export const MAX_REPLAY_LIVE_EVENTS = 128;
export const MAX_REPLAY_LIVE_BYTES = 8 * 1024 * 1024;
export const DEFAULT_REPLAY_PAGE_LIMIT = 256;
export const MAX_REPLAY_PAGE_LIMIT = 1024;
export const MAX_KNOWN_SESSION_CURSORS = 100_000;

export type SessionBinding = {
	plotId: string;
	sessionId: string;
};

export type SessionEndpoint = {
	kind: typeof SESSION_ENDPOINT_KIND;
	transport: "unix";
	address: string;
	state: string;
};

export type SessionCommand = {
	kind: typeof SESSION_COMMAND_KIND;
	command_id: string;
	plot_id: string;
	session_id: string;
	command: { type: "prompt"; text: string; [key: string]: unknown };
	[key: string]: unknown;
};

export type SessionEventPayload = DurableSessionEventPayload;
export type SessionEvent = DurableSessionEvent;

export type SessionSubscribe = {
	kind: typeof SESSION_SUBSCRIBE_KIND;
	request_id: string;
	plot_id: string;
	session_id: string;
	after_cursor: string | null;
	page_limit: number;
	[key: string]: unknown;
};

export type SessionReplayComplete = {
	kind: typeof SESSION_REPLAY_COMPLETE_KIND;
	request_id: string;
	plot_id: string;
	session_id: string;
	stream_id: string;
	cursor: string | null;
	sequence: number;
	event_count: number;
};

export type SessionFeedErrorCode =
	| "history_gap"
	| "history_corrupt"
	| "foreign_session"
	| "branch_diverged"
	| "history_too_large"
	| "cursor_conflict"
	| "command_conflict"
	| "replay_buffer_overflow"
	| "protocol_violation"
	| "unavailable"
	| "internal";

export type SessionFeedError = {
	kind: typeof SESSION_FEED_ERROR_KIND;
	request_id: string | null;
	plot_id: string | null;
	session_id: string | null;
	code: SessionFeedErrorCode;
	retryable: boolean;
	message: string;
};

export type SessionFeedFrame = DurableSessionEvent | SessionReplayComplete | SessionFeedError;

export type SessionCommandResult =
	| { kind: "accepted" }
	| { kind: "duplicate" }
	| { kind: "error"; error: Omit<SessionFeedError, "kind" | "request_id" | "plot_id" | "session_id"> };

type ProtocolEffects = {
	activeBranch?: readonly PiSessionEntry[];
	abandonedCursors?: readonly string[];
	appendKind?: DurableSessionEventKind;
	appendEntry(customType: typeof SESSION_EVENT_ENTRY, data: DurableSessionEvent): void;
	sendUserMessage(text: string): Promise<void>;
	defer?(task: () => void | Promise<void>): void;
	emit?(event: DurableSessionEvent): void;
	nextId?(): string;
};

type AssistantLike = {
	role?: unknown;
	content?: unknown;
	errorMessage?: unknown;
};

type QueuedStructuredTurn = {
	source: "structured";
	commandId: string;
	text: string;
};

function isObject(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isSafeIdentity(value: unknown): value is string {
	return typeof value === "string"
		&& value.trim().length > 0
		&& Buffer.byteLength(value, "utf8") <= MAX_SESSION_IDENTITY_BYTES
		&& !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

export function retainRecentSessionCursors(
	known: Set<string>,
	cursors: Iterable<string>,
	limit = MAX_KNOWN_SESSION_CURSORS,
): void {
	if (!Number.isSafeInteger(limit) || limit < 1 || limit > MAX_KNOWN_SESSION_CURSORS) {
		throw new Error(`known Session cursor limit must be between 1 and ${MAX_KNOWN_SESSION_CURSORS}`);
	}
	for (const cursor of cursors) {
		known.delete(cursor);
		known.add(cursor);
	}
	while (known.size > limit) {
		const oldest = known.values().next().value;
		if (typeof oldest !== "string") break;
		known.delete(oldest);
	}
}

export function parseCommand(value: unknown): SessionCommand | undefined {
	if (!isObject(value) || value.kind !== SESSION_COMMAND_KIND) return undefined;
	if (!isSafeIdentity(value.command_id) || !isSafeIdentity(value.plot_id) || !isSafeIdentity(value.session_id)) return undefined;
	if (!isObject(value.command) || value.command.type !== "prompt") return undefined;
	if (typeof value.command.text !== "string" || value.command.text.trim().length === 0) return undefined;
	return {
		...value,
		kind: SESSION_COMMAND_KIND,
		command_id: value.command_id,
		plot_id: value.plot_id,
		session_id: value.session_id,
		command: { ...value.command, type: "prompt", text: value.command.text },
	};
}

export function parseSubscribe(value: unknown): SessionSubscribe | undefined {
	if (!isObject(value) || value.kind !== SESSION_SUBSCRIBE_KIND) return undefined;
	if (!isSafeIdentity(value.request_id) || !isSafeIdentity(value.plot_id) || !isSafeIdentity(value.session_id)) return undefined;
	if (!Object.hasOwn(value, "after_cursor")) return undefined;
	if (value.after_cursor !== null && !isSafeIdentity(value.after_cursor)) return undefined;
	const pageLimit = value.page_limit === undefined ? DEFAULT_REPLAY_PAGE_LIMIT : value.page_limit;
	if (!Number.isSafeInteger(pageLimit) || (pageLimit as number) < 1 || (pageLimit as number) > MAX_REPLAY_PAGE_LIMIT) return undefined;
	return {
		...value,
		kind: SESSION_SUBSCRIBE_KIND,
		request_id: value.request_id,
		plot_id: value.plot_id,
		session_id: value.session_id,
		after_cursor: value.after_cursor as string | null,
		page_limit: pageLimit as number,
	};
}

function assistantText(message: AssistantLike): string | undefined {
	if (message.role !== "assistant" || !Array.isArray(message.content)) return undefined;
	const text = message.content
		.filter((part): part is { type: "text"; text: string } => isObject(part) && part.type === "text" && typeof part.text === "string")
		.map((part) => part.text)
		.join("");
	return text.length > 0 ? text : undefined;
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function resultError(code: SessionFeedErrorCode, message: string, retryable = false): SessionCommandResult {
	return { kind: "error", error: { code, retryable, message } };
}

function mapLogError(error: DurableSessionLogError): {
	code: SessionFeedErrorCode;
	retryable: boolean;
	message: string;
} {
	const code: SessionFeedErrorCode = (() => {
		switch (error.code) {
			case "history_gap": return "history_gap";
			case "history_corrupt":
			case "malformed_history": return "history_corrupt";
			case "foreign_history":
			case "foreign_cursor": return "foreign_session";
			case "branch_divergence": return "branch_diverged";
			case "history_too_large":
			case "event_too_large": return "history_too_large";
			case "duplicate_event":
			case "duplicate_command_event": return "cursor_conflict";
			case "command_conflict": return "command_conflict";
			case "malformed_cursor":
			case "invalid_limit":
			case "invalid_binding": return "protocol_violation";
			case "persistence_failed": return "internal";
			default: return "internal";
		}
	})();
	return { code, retryable: false, message: error.message };
}

function boundedFeedMessage(message: string): string {
	if (message.length === 0) return "Session feed error";
	if (Buffer.byteLength(message, "utf8") <= 4096) return message;
	let prefix = "";
	let bytes = 3;
	for (const character of message) {
		const characterBytes = Buffer.byteLength(character, "utf8");
		if (bytes + characterBytes > 4096) break;
		prefix += character;
		bytes += characterBytes;
	}
	return `${prefix}...`;
}

/**
 * One exact Plot/Session durable journal plus Pi outer-loop correlation.
 * The log commits before emit and before sendUserMessage.
 */
export class SessionProtocolEngine {
	readonly binding: SessionBinding;
	readonly log: DurableSessionLog;
	readonly #effects: ProtocolEffects;
	readonly #structuredQueue: QueuedStructuredTurn[] = [];
	readonly #listeners = new Set<(event: DurableSessionEvent) => void>();
	readonly #assistantText: string[] = [];
	readonly #assistantErrors: string[] = [];
	#activeTurn?: QueuedStructuredTurn;
	#deliveredStructuredTurn?: QueuedStructuredTurn;
	#deliveryAttemptInFlight = false;
	#deliveryScheduled = false;
	#agentActive = false;
	#closed = false;
	#started = false;

	constructor(binding: SessionBinding, effects: ProtocolEffects) {
		if (!isSafeIdentity(binding.plotId) || !isSafeIdentity(binding.sessionId)) {
			throw new Error("invalid Nopal Session binding identity");
		}
		this.binding = { ...binding };
		this.#effects = effects;
		this.log = DurableSessionLog.hydrate({
			binding,
			activeBranch: effects.activeBranch ?? [],
			abandonedCursors: effects.abandonedCursors,
			appendEntry: effects.appendEntry,
			appendKind: effects.appendKind ?? SESSION_EVENT_V3_KIND,
		});
	}

	start(): DurableSessionEvent {
		if (this.#started) {
			const ready = this.log.ready();
			if (!ready) throw new Error("started durable Session has no ready event");
			return ready;
		}
		this.#started = true;
		const existing = this.log.ready();
		const ready = this.log.ensureReady((this.#effects.nextId ?? randomUUID)());
		if (!existing) this.#emit(ready);
		this.#recordInterruptedCommands();
		return ready;
	}

	subscribe(listener: (event: DurableSessionEvent) => void): () => void {
		this.#listeners.add(listener);
		return () => this.#listeners.delete(listener);
	}

	close(): void {
		this.#closed = true;
		this.#structuredQueue.length = 0;
		this.#deliveredStructuredTurn = undefined;
		this.#deliveryScheduled = false;
	}

	async accept(value: unknown): Promise<SessionCommandResult> {
		if (this.#closed) return resultError("unavailable", "Session host is closed", true);
		const command = parseCommand(value);
		if (!command) return resultError("protocol_violation", "invalid nopal.session.command/v1 command");
		if (command.plot_id !== this.binding.plotId || command.session_id !== this.binding.sessionId) {
			return resultError("foreign_session", "command identity does not match this Plot Session");
		}
		try {
			const disposition = this.log.classifyCommand(command.command_id, command.command.text);
			if (disposition.kind === "duplicate") return { kind: "duplicate" };
			this.#publish({ type: "user_message", text: command.command.text }, command.command_id);
			this.#structuredQueue.push({
				source: "structured",
				commandId: command.command_id,
				text: command.command.text,
			});
			await this.#pumpStructuredDelivery();
			return { kind: "accepted" };
		} catch (error) {
			if (error instanceof DurableSessionLogError) {
				const mapped = mapLogError(error);
				return { kind: "error", error: mapped };
			}
			return resultError("internal", errorMessage(error));
		}
	}

	observeInput(_text: string, _source: InputSource): undefined {
		return undefined;
	}

	observeAgentStart(): void {
		if (this.#closed || this.#agentActive) return;
		this.#agentActive = true;
		this.#activeTurn = this.#deliveredStructuredTurn;
		this.#deliveredStructuredTurn = undefined;
	}

	activeCommandId(): string | undefined {
		return this.#activeTurn?.commandId;
	}

	publishActivity(input: AppendSessionEvent): DurableSessionEvent {
		const type = (input.event as { type?: unknown } | undefined)?.type;
		if (![
			"command_started",
			"command_finished",
			"command_failed",
			"tool_started",
			"tool_finished",
			"tool_failed",
		].includes(String(type))) {
			throw new Error("Session activity publisher accepts only typed activity payloads");
		}
		const envelope = this.log.append(input);
		this.#emit(envelope);
		return envelope;
	}

	observeAssistant(message: AssistantLike): void {
		if (this.#closed) return;
		const text = assistantText(message);
		if (text !== undefined) this.#assistantText.push(text);
		if (typeof message.errorMessage === "string" && message.errorMessage.length > 0) {
			this.#assistantErrors.push(message.errorMessage);
		}
	}

	observeAgentEnd(): void {
		if (this.#closed || !this.#agentActive) return;
		const commandId = this.#activeTurn?.commandId;
		const text = this.#assistantText.join("\n\n");
		const errors = this.#assistantErrors.join("\n");
		this.#assistantText.length = 0;
		this.#assistantErrors.length = 0;
		this.#activeTurn = undefined;
		this.#agentActive = false;
		if (commandId !== undefined) {
			if (text.length > 0) this.#publish({ type: "assistant_message", text }, commandId);
			if (errors.length > 0) this.protocolError(errors, commandId);
		}
		this.#scheduleStructuredDelivery();
	}

	protocolError(message: string, commandId?: string): DurableSessionEvent {
		return this.#publish({ type: "session_error", message }, commandId);
	}

	#recordInterruptedCommands(): void {
		const events = this.log.events();
		const completed = new Set<string>();
		for (const event of events) {
			if (
				event.command_id
				&& (event.event.type === "assistant_message" || event.event.type === "session_error")
			) completed.add(event.command_id);
		}
		for (const event of events) {
			if (
				event.event.type === "user_message"
				&& event.command_id
				&& !completed.has(event.command_id)
			) {
				this.protocolError(
					"Pi host restarted before this accepted command produced a terminal assistant or error event; the command was not redelivered.",
					event.command_id,
				);
				completed.add(event.command_id);
			}
		}
	}

	#scheduleStructuredDelivery(): void {
		if (this.#closed || this.#deliveryScheduled || this.#structuredQueue.length === 0) return;
		this.#deliveryScheduled = true;
		const run = async () => {
			this.#deliveryScheduled = false;
			await this.#pumpStructuredDelivery();
		};
		if (this.#effects.defer) {
			this.#effects.defer(run);
		} else {
			setTimeout(() => { void run(); }, 0);
		}
	}

	async #pumpStructuredDelivery(): Promise<void> {
		if (
			this.#closed
			|| this.#agentActive
			|| this.#deliveryAttemptInFlight
			|| this.#deliveredStructuredTurn
		) return;
		while (this.#structuredQueue.length > 0 && !this.#agentActive) {
			const turn = this.#structuredQueue.shift();
			if (!turn) return;
			this.#deliveryAttemptInFlight = true;
			this.#deliveredStructuredTurn = turn;
			try {
				await this.#effects.sendUserMessage(turn.text);
				return;
			} catch (error) {
				if (this.#closed) return;
				if (this.#deliveredStructuredTurn?.commandId === turn.commandId) {
					this.#deliveredStructuredTurn = undefined;
				}
				if (this.#activeTurn?.commandId === turn.commandId) this.#activeTurn = undefined;
				this.protocolError(`Pi rejected the accepted prompt: ${errorMessage(error)}`, turn.commandId);
			} finally {
				this.#deliveryAttemptInFlight = false;
			}
		}
	}

	#publish(event: DurableSessionEventPayload, commandId?: string): DurableSessionEvent {
		const envelope = this.log.append({
			eventId: (this.#effects.nextId ?? randomUUID)(),
			commandId,
			event,
		});
		this.#emit(envelope);
		return envelope;
	}

	#emit(event: DurableSessionEvent): void {
		this.#effects.emit?.(event);
		for (const listener of this.#listeners) listener(event);
	}
}

/** Strict LF-only, bounded JSONL decoder shared by every socket client. */
export class JsonlDecoder {
	readonly #onRecord: (record: unknown) => void;
	readonly #onError: (message: string) => void;
	readonly #utf8 = new TextDecoder("utf-8", { fatal: true });
	#buffer = Buffer.alloc(0);
	#discardingOversizedLine = false;
	#finished = false;

	constructor(onRecord: (record: unknown) => void, onError: (message: string) => void) {
		this.#onRecord = onRecord;
		this.#onError = onError;
	}

	push(chunk: Buffer): void {
		if (this.#finished) return;
		if (this.#discardingOversizedLine) {
			const newline = chunk.indexOf(0x0a);
			if (newline < 0) return;
			this.#discardingOversizedLine = false;
			chunk = chunk.subarray(newline + 1);
		}
		this.#buffer = Buffer.concat([this.#buffer, chunk]);
		while (true) {
			const newline = this.#buffer.indexOf(0x0a);
			if (newline < 0) break;
			const line = this.#buffer.subarray(0, newline);
			this.#buffer = this.#buffer.subarray(newline + 1);
			if (line.length > MAX_JSONL_LINE_BYTES) {
				this.#onError("nopal Session JSONL line exceeds the 1 MiB limit");
				continue;
			}
			if (line.includes(0x0d)) {
				this.#onError("nopal Session protocol requires strict LF framing");
				continue;
			}
			if (line.length === 0) {
				this.#onError("nopal Session protocol does not accept empty JSONL records");
				continue;
			}
			let text: string;
			try {
				text = this.#utf8.decode(line);
			} catch {
				this.#onError("invalid UTF-8 in nopal Session frame");
				continue;
			}
			try {
				this.#onRecord(JSON.parse(text));
			} catch {
				this.#onError("invalid JSON in nopal Session frame");
			}
		}
		if (this.#buffer.length > MAX_JSONL_LINE_BYTES) {
			this.#buffer = Buffer.alloc(0);
			this.#discardingOversizedLine = true;
			this.#onError("nopal Session JSONL line exceeds the 1 MiB limit");
		}
	}

	finish(): boolean {
		if (this.#finished) return false;
		this.#finished = true;
		if (this.#buffer.length > 0) {
			this.#buffer = Buffer.alloc(0);
			this.#onError("nopal Session protocol received an unterminated JSONL record at EOF");
			this.#discardingOversizedLine = false;
			return true;
		}
		this.#discardingOversizedLine = false;
		return false;
	}
}

type FeedConnectionOptions = {
	engine: SessionProtocolEngine;
	send(frame: SessionFeedFrame): void | Promise<void>;
	fail?(frame: SessionFeedError): void | Promise<void>;
	finish?(): void | Promise<void>;
	close(): void | Promise<void>;
	replayYield?: () => void | Promise<void>;
};

export class SessionFeedConnection {
	readonly #engine: SessionProtocolEngine;
	readonly #sendEffect: FeedConnectionOptions["send"];
	readonly #failEffect?: FeedConnectionOptions["fail"];
	readonly #finishEffect: NonNullable<FeedConnectionOptions["finish"]>;
	readonly #closeEffect: FeedConnectionOptions["close"];
	readonly #replayYield?: FeedConnectionOptions["replayYield"];
	readonly #liveBuffer: DurableSessionEvent[] = [];
	readonly #unsubscribe: () => void;
	#requestId: string | null = null;
	#liveBytes = 0;
	#subscribed = false;
	#replaying = false;
	#closed = false;

	constructor(options: FeedConnectionOptions) {
		this.#engine = options.engine;
		this.#sendEffect = options.send;
		this.#failEffect = options.fail;
		this.#finishEffect = options.finish ?? options.close;
		this.#closeEffect = options.close;
		this.#replayYield = options.replayYield;
		this.#unsubscribe = this.#engine.subscribe((event) => this.#onLive(event));
	}

	async accept(record: unknown): Promise<void> {
		if (this.#closed) return;
		if (!this.#subscribed) {
			const request = parseSubscribe(record);
			if (!request) {
				await this.protocolViolation("client must send one valid subscribe frame before commands");
				return;
			}
			this.#requestId = request.request_id;
			if (request.plot_id !== this.#engine.binding.plotId || request.session_id !== this.#engine.binding.sessionId) {
				await this.#fatal("foreign_session", "subscription identity does not match this Plot Session");
				return;
			}
			await this.#replay(request);
			return;
		}
		if (this.#replaying) {
			await this.protocolViolation("commands are not accepted before replay_complete");
			return;
		}
		if (parseSubscribe(record)) {
			await this.protocolViolation("one client connection may subscribe only once");
			return;
		}
		const result = await this.#engine.accept(record);
		if (result.kind === "error") {
			await this.#fatal(result.error.code, result.error.message, result.error.retryable);
		}
	}

	async protocolViolation(message: string): Promise<void> {
		await this.#fatal("protocol_violation", message);
	}

	close(): void {
		if (!this.#beginClose()) return;
		void this.#closeEffect();
	}

	async #replay(request: SessionSubscribe): Promise<void> {
		this.#subscribed = true;
		this.#replaying = true;
		try {
			this.#engine.log.eventsAfter(request.after_cursor, request.page_limit);
			const snapshot = this.#engine.log.events();
			const snapshotSequence = snapshot.length;
			const snapshotCursor = snapshot.at(-1)?.cursor ?? null;
			const start = request.after_cursor === null
				? 0
				: snapshot.findIndex((event) => event.cursor === request.after_cursor) + 1;
			let emitted = 0;
			for (let offset = start; offset < snapshot.length; offset += request.page_limit) {
				for (const event of snapshot.slice(offset, offset + request.page_limit)) {
					if (this.#closed) return;
					await this.#send(event);
					emitted += 1;
				}
				await (this.#replayYield?.() ?? Promise.resolve());
				if (this.#closed) return;
			}
			await this.#send({
				kind: SESSION_REPLAY_COMPLETE_KIND,
				request_id: request.request_id,
				plot_id: this.#engine.binding.plotId,
				session_id: this.#engine.binding.sessionId,
				stream_id: this.#engine.log.streamId,
				cursor: snapshotCursor,
				sequence: snapshotSequence,
				event_count: emitted,
			});
			while (this.#liveBuffer.length > 0) {
				const event = this.#liveBuffer[0];
				if (!event) break;
				if (this.#closed) return;
				await this.#send(event);
				if (this.#closed) return;
				this.#liveBuffer.shift();
				this.#liveBytes -= Buffer.byteLength(JSON.stringify(event), "utf8");
			}
			this.#replaying = false;
		} catch (error) {
			if (error instanceof DurableSessionLogError) {
				const mapped = mapLogError(error);
				await this.#fatal(mapped.code, mapped.message, mapped.retryable);
			} else {
				await this.#fatal("unavailable", `could not stream Session replay: ${errorMessage(error)}`, true);
			}
		}
	}

	#onLive(event: DurableSessionEvent): void {
		if (this.#closed || !this.#subscribed) return;
		if (!this.#replaying) {
			void this.#send(event).catch((error) =>
				this.#fatal("unavailable", `could not write live Session event: ${errorMessage(error)}`, true));
			return;
		}
		const bytes = Buffer.byteLength(JSON.stringify(event), "utf8");
		if (
			this.#liveBuffer.length >= MAX_REPLAY_LIVE_EVENTS
			|| this.#liveBytes + bytes > MAX_REPLAY_LIVE_BYTES
		) {
			void this.#fatal("replay_buffer_overflow", "live Session events exceeded the bounded replay buffer", true);
			return;
		}
		this.#liveBuffer.push(event);
		this.#liveBytes += bytes;
	}

	async #fatal(code: SessionFeedErrorCode, message: string, retryable = false): Promise<void> {
		if (!this.#beginClose()) return;
		const frame: SessionFeedError = {
			kind: SESSION_FEED_ERROR_KIND,
			request_id: this.#requestId,
			plot_id: this.#engine.binding.plotId,
			session_id: this.#engine.binding.sessionId,
			code,
			retryable,
			message: boundedFeedMessage(message),
		};
		try {
			if (this.#failEffect) {
				await this.#failEffect(frame);
			} else {
				await this.#sendEffect(frame);
				await this.#finishEffect();
			}
		} catch {
			await this.#closeEffect();
		}
	}

	async #send(frame: SessionFeedFrame): Promise<void> {
		if (!this.#closed) await this.#sendEffect(frame);
	}

	#beginClose(): boolean {
		if (this.#closed) return false;
		this.#closed = true;
		this.#unsubscribe();
		this.#liveBuffer.length = 0;
		this.#liveBytes = 0;
		return true;
	}
}

const MAX_SOCKET_QUEUE_FRAMES = 128;
const MAX_SOCKET_QUEUE_BYTES = 8 * 1024 * 1024;
const SOCKET_DRAIN_TIMEOUT_MS = 2_000;

type SocketWriteOperation = {
	line: string;
	bytes: number;
	control: boolean;
	resolve(): void;
	reject(error: Error): void;
};

class SocketFrameWriter {
	readonly #socket: Socket;
	readonly #queue: SocketWriteOperation[] = [];
	readonly #idleWaiters = new Set<() => void>();
	#active?: SocketWriteOperation;
	#queuedFrames = 0;
	#queuedBytes = 0;
	#accepting = true;
	#pumping = false;
	#failureDeadline?: number;
	#fatal?: Promise<void>;

	constructor(socket: Socket) {
		this.#socket = socket;
	}

	send(frame: SessionFeedFrame): Promise<void> {
		if (!this.#accepting || this.#socket.destroyed) {
			return Promise.reject(new Error("Session feed socket is closed"));
		}
		const line = `${JSON.stringify(frame)}\n`;
		const bytes = Buffer.byteLength(line, "utf8");
		if (
			this.#queuedFrames >= MAX_SOCKET_QUEUE_FRAMES
			|| this.#queuedBytes + bytes > MAX_SOCKET_QUEUE_BYTES
		) {
			const error = new Error("Session feed socket exceeded its bounded output queue");
			this.#beginFailure(error);
			return Promise.reject(error);
		}
		return this.#enqueue(line, bytes, false);
	}

	fail(frame: SessionFeedError): Promise<void> {
		if (this.#fatal) return this.#fatal;
		const deadline = this.#failureDeadline ?? Date.now() + SOCKET_DRAIN_TIMEOUT_MS;
		this.#failureDeadline = deadline;
		this.#accepting = false;
		this.#cancelQueuedData(new Error("Session feed socket cancelled queued data after an output failure"));
		const line = `${JSON.stringify(frame)}\n`;
		const operation = this.#enqueue(line, Buffer.byteLength(line, "utf8"), true)
			.then(() => this.#end(deadline));
		this.#fatal = operation;
		return operation;
	}

	async finish(): Promise<void> {
		this.#accepting = false;
		const deadline = Date.now() + SOCKET_DRAIN_TIMEOUT_MS;
		await this.#waitForIdle(deadline);
		await this.#end(deadline);
	}

	close(): void {
		this.#accepting = false;
		this.#cancelQueue(new Error("Session feed socket closed"));
		this.#socket.destroy();
	}

	#enqueue(line: string, bytes: number, control: boolean): Promise<void> {
		if (this.#socket.destroyed) return Promise.reject(new Error("Session feed socket is closed"));
		return new Promise<void>((resolve, reject) => {
			const operation = { line, bytes, control, resolve, reject };
			this.#queue.push(operation);
			if (!control) {
				this.#queuedFrames += 1;
				this.#queuedBytes += bytes;
			}
			void this.#pump();
		});
	}

	async #pump(): Promise<void> {
		if (this.#pumping) return;
		this.#pumping = true;
		try {
			while (this.#queue.length > 0) {
				const operation = this.#queue.shift();
				if (!operation) break;
				this.#active = operation;
				let failure: Error | undefined;
				try {
					await this.#write(operation.line, this.#failureDeadline);
				} catch (error) {
					failure = error instanceof Error ? error : new Error(String(error));
				}
				this.#active = undefined;
				if (!operation.control) {
					this.#queuedFrames -= 1;
					this.#queuedBytes -= operation.bytes;
				}
				if (failure) {
					if (!operation.control) this.#beginFailure(failure);
					operation.reject(failure);
				} else {
					operation.resolve();
				}
			}
		} finally {
			this.#pumping = false;
			this.#notifyIdle();
			if (this.#queue.length > 0) void this.#pump();
		}
	}

	#beginFailure(error: Error): void {
		this.#accepting = false;
		this.#failureDeadline ??= Date.now() + SOCKET_DRAIN_TIMEOUT_MS;
		this.#cancelQueuedData(error);
	}

	#cancelQueuedData(error: Error): void {
		for (let index = this.#queue.length - 1; index >= 0; index -= 1) {
			const operation = this.#queue[index];
			if (!operation || operation.control) continue;
			this.#queue.splice(index, 1);
			this.#queuedFrames -= 1;
			this.#queuedBytes -= operation.bytes;
			operation.reject(error);
		}
		this.#notifyIdle();
	}

	#cancelQueue(error: Error): void {
		for (const operation of this.#queue.splice(0)) {
			if (!operation.control) {
				this.#queuedFrames -= 1;
				this.#queuedBytes -= operation.bytes;
			}
			operation.reject(error);
		}
		this.#notifyIdle();
	}

	async #waitForIdle(deadline: number): Promise<void> {
		if (!this.#active && this.#queue.length === 0 && !this.#pumping) return;
		await new Promise<void>((resolve, reject) => {
			let settled = false;
			const finish = (error?: Error) => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				this.#idleWaiters.delete(onIdle);
				error ? reject(error) : resolve();
			};
			const onIdle = () => finish();
			const timer = setTimeout(
				() => finish(new Error("Session feed socket flush deadline expired")),
				Math.max(0, deadline - Date.now()),
			);
			this.#idleWaiters.add(onIdle);
		});
	}

	#notifyIdle(): void {
		if (this.#active || this.#queue.length > 0 || this.#pumping) return;
		for (const waiter of [...this.#idleWaiters]) waiter();
	}

	async #end(deadline: number): Promise<void> {
		if (this.#socket.destroyed || this.#socket.writableEnded) return;
		await new Promise<void>((resolve, reject) => {
			let settled = false;
			const finish = (error?: Error) => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				this.#socket.off("error", onError);
				error ? reject(error) : resolve();
			};
			const onError = (error: Error) => finish(error);
			const timer = setTimeout(
				() => finish(new Error("Session feed socket flush deadline expired")),
				Math.max(0, deadline - Date.now()),
			);
			this.#socket.once("error", onError);
			this.#socket.end(() => finish());
		});
	}

	async #write(line: string, deadline?: number): Promise<void> {
		if (this.#socket.destroyed || !this.#socket.writable) {
			throw new Error("Session feed socket is not writable");
		}
		if (this.#socket.write(line)) return;
		await new Promise<void>((resolve, reject) => {
			let settled = false;
			const finish = (error?: Error) => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				this.#socket.off("drain", onDrain);
				this.#socket.off("error", onError);
				this.#socket.off("close", onClose);
				error ? reject(error) : resolve();
			};
			const onDrain = () => finish();
			const onError = (error: Error) => finish(error);
			const onClose = () => finish(new Error("Session feed socket closed before drain"));
			const timer = setTimeout(
				() => finish(new Error("Session feed socket backpressure timed out")),
				Math.max(0, (deadline ?? Date.now() + SOCKET_DRAIN_TIMEOUT_MS) - Date.now()),
			);
			this.#socket.once("drain", onDrain);
			this.#socket.once("error", onError);
			this.#socket.once("close", onClose);
		});
	}
}

type OwnedPath = { dev: number | bigint; ino: number | bigint };

type SessionBridgeStartupFault = {
	binding: SessionBinding;
	code: SessionFeedErrorCode;
	retryable: false;
	message: string;
};

type SessionBridgeClient = {
	close(): void;
};

class UnixSessionServer {
	readonly path: string;
	readonly #createClient: (socket: Socket) => SessionBridgeClient;
	readonly #clients = new Map<Socket, SessionBridgeClient>();
	#server?: Server;
	#ownedPath?: OwnedPath;

	constructor(path: string, createClient: (socket: Socket) => SessionBridgeClient) {
		this.path = path;
		this.#createClient = createClient;
	}

	async start(): Promise<void> {
		if (this.#server) return;
		await mkdir(dirname(this.path), { recursive: true, mode: 0o700 });
		await chmod(dirname(this.path), 0o700);
		const server = createServer({ allowHalfOpen: true }, (socket) => this.#acceptClient(socket));
		this.#server = server;
		try {
			await new Promise<void>((resolve, reject) => {
				const onError = (error: Error) => reject(error);
				server.once("error", onError);
				server.listen(this.path, () => {
					server.off("error", onError);
					resolve();
				});
			});
			await chmod(this.path, 0o600);
			const stat = await lstat(this.path);
			this.#ownedPath = { dev: stat.dev, ino: stat.ino };
		} catch (error) {
			await this.close().catch(() => undefined);
			throw error;
		}
	}

	async close(): Promise<void> {
		for (const client of this.#clients.values()) client.close();
		this.#clients.clear();
		const server = this.#server;
		this.#server = undefined;
		if (server?.listening) await new Promise<void>((resolve) => server.close(() => resolve()));
		const owned = this.#ownedPath;
		this.#ownedPath = undefined;
		if (!owned) return;
		try {
			const current = await lstat(this.path);
			if (current.dev === owned.dev && current.ino === owned.ino) await unlink(this.path);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
	}

	#acceptClient(socket: Socket): void {
		const client = this.#createClient(socket);
		this.#clients.set(socket, client);
		const cleanup = () => {
			this.#clients.delete(socket);
			client.close();
		};
		socket.on("close", cleanup);
		socket.on("error", cleanup);
	}
}

export class NopalSessionBridge {
	readonly path: string;
	readonly engine: SessionProtocolEngine;
	readonly #server: UnixSessionServer;

	constructor(options: { path: string; engine: SessionProtocolEngine }) {
		this.path = options.path;
		this.engine = options.engine;
		this.#server = new UnixSessionServer(this.path, (socket) => this.#acceptClient(socket));
	}

	endpoint(): SessionEndpoint {
		return { kind: SESSION_ENDPOINT_KIND, transport: "unix", address: this.path, state: "ready" };
	}

	async start(): Promise<void> {
		try {
			await this.#server.start();
			this.engine.start();
		} catch (error) {
			await this.close().catch(() => undefined);
			throw error;
		}
	}

	async close(): Promise<void> {
		this.engine.close();
		await this.#server.close();
	}

	#acceptClient(socket: Socket): SessionFeedConnection {
		const writer = new SocketFrameWriter(socket);
		const feed = new SessionFeedConnection({
			engine: this.engine,
			send: (frame) => writer.send(frame),
			fail: (frame) => writer.fail(frame),
			finish: () => writer.finish(),
			close: () => writer.close(),
		});
		const decoder = new JsonlDecoder(
			(record) => { void feed.accept(record); },
			(message) => { void feed.protocolViolation(message); },
		);
		socket.on("data", (chunk) => decoder.push(chunk));
		socket.on("end", () => {
			if (!decoder.finish()) feed.close();
		});
		return feed;
	}
}

class NopalSessionFaultBridge {
	readonly path: string;
	readonly #fault: SessionBridgeStartupFault;
	readonly #server: UnixSessionServer;

	constructor(options: { path: string; fault: SessionBridgeStartupFault }) {
		this.path = options.path;
		this.#fault = options.fault;
		this.#server = new UnixSessionServer(this.path, (socket) => this.#acceptClient(socket));
	}

	endpoint(): SessionEndpoint {
		return { kind: SESSION_ENDPOINT_KIND, transport: "unix", address: this.path, state: "ready" };
	}

	async start(): Promise<void> {
		await this.#server.start();
	}

	async close(): Promise<void> {
		await this.#server.close();
	}

	#acceptClient(socket: Socket): SocketFrameWriter {
		const writer = new SocketFrameWriter(socket);
		void writer.fail({
			kind: SESSION_FEED_ERROR_KIND,
			request_id: null,
			plot_id: this.#fault.binding.plotId,
			session_id: this.#fault.binding.sessionId,
			code: this.#fault.code,
			retryable: this.#fault.retryable,
			message: boundedFeedMessage(this.#fault.message),
		}).catch(() => writer.close());
		return writer;
	}
}

export function defaultSessionSocketPath(binding: SessionBinding, runtimeRoot?: string): string {
	const uid = typeof process.getuid === "function" ? process.getuid() : "user";
	const digest = createHash("sha256").update(`${binding.plotId}\0${binding.sessionId}`).digest("hex").slice(0, 20);
	return join(runtimeRoot ?? join(tmpdir(), `nopal-${uid}`), `session-${digest}.sock`);
}

export type SessionHistorySource = {
	getBranch(): readonly PiSessionEntry[];
};

export type SessionBridgeRegistration = {
	endpoint(): SessionEndpoint | undefined;
	bind(binding: SessionBinding, history?: SessionHistorySource): Promise<SessionEndpoint | undefined>;
	refresh(cwd: string): Promise<SessionEndpoint | undefined>;
	close(): Promise<void>;
};

export type SessionBridgeAdapter = {
	endpoint(): SessionEndpoint;
	start(): Promise<void>;
	close(): Promise<void>;
};

export type SessionBridgeFactory = (options: {
	path: string;
	engine: SessionProtocolEngine;
}) => SessionBridgeAdapter;

export function registerNopalSessionBridge(
	pi: ExtensionAPI,
	exec: ExecFn,
	options: {
		runtimeRoot?: string;
		paneId?: string;
		history?: SessionHistorySource;
		bridgeFactory?: SessionBridgeFactory;
		activityDiagnostic?(message: string): void;
	} = {},
): SessionBridgeRegistration {
	let bridge: SessionBridgeAdapter | undefined;
	let engine: SessionProtocolEngine | undefined;
	let activityProducer: SessionActivityProducer | undefined;
	let activeBinding: SessionBinding | undefined;
	let activeHistory = options.history;
	let transition: Promise<void> = Promise.resolve();
	let lifecycleGeneration = 0;
	let stopped = false;
	let permanentlyClosed = false;
	const knownCursors = new Map<string, Set<string>>();

	const bindingKey = (binding: SessionBinding) => `${binding.plotId}\0${binding.sessionId}`;
	const rememberEngine = () => {
		if (!engine || !activeBinding) return;
		const known = knownCursors.get(bindingKey(activeBinding)) ?? new Set<string>();
		retainRecentSessionCursors(known, engine.log.events().map((event) => event.cursor));
		knownCursors.set(bindingKey(activeBinding), known);
	};
	const serialize = <T>(operation: () => Promise<T>): Promise<T> => {
		const result = transition.then(operation);
		transition = result.then(() => undefined, () => undefined);
		return result;
	};
	const closeActive = async () => {
		rememberEngine();
		const current = bridge;
		bridge = undefined;
		engine = undefined;
		activityProducer = undefined;
		activeBinding = undefined;
		await current?.close();
	};
	const historyFromContext = (ctx: unknown): SessionHistorySource | undefined => {
		const manager = (ctx as { sessionManager?: unknown } | undefined)?.sessionManager;
		return isObject(manager) && typeof manager.getBranch === "function"
			? manager as unknown as SessionHistorySource
			: undefined;
	};
	const createEngine = (binding: SessionBinding, branch: readonly PiSessionEntry[]): SessionProtocolEngine => {
		const effects = {
			activeBranch: branch,
			appendKind: SESSION_EVENT_V3_KIND,
			appendEntry: (customType: typeof SESSION_EVENT_ENTRY, data: DurableSessionEvent) => pi.appendEntry(customType, data),
			sendUserMessage: async (text: string) => { await pi.sendUserMessage(text); },
			defer: (task: () => void | Promise<void>) => { setTimeout(() => { void task(); }, 0); },
		};
		const probe = new SessionProtocolEngine(binding, effects);
		const active = new Set(probe.log.events().map((event) => event.cursor));
		const known = knownCursors.get(bindingKey(binding)) ?? new Set<string>();
		const abandoned = [...known].filter((cursor) => !active.has(cursor));
		return abandoned.length === 0
			? probe
			: new SessionProtocolEngine(binding, { ...effects, abandonedCursors: abandoned });
	};
	const bindNow = async (
		binding: SessionBinding,
		history?: SessionHistorySource,
		force = false,
	): Promise<SessionEndpoint | undefined> => {
		if (stopped || permanentlyClosed) return undefined;
		if (history) activeHistory = history;
		if (!force && bridge && activeBinding?.plotId === binding.plotId && activeBinding.sessionId === binding.sessionId) {
			return bridge.endpoint();
		}
		await closeActive();
		if (stopped || permanentlyClosed) return undefined;
		let nextEngine: SessionProtocolEngine;
		try {
			nextEngine = createEngine(binding, activeHistory?.getBranch() ?? []);
		} catch (error) {
			if (!(error instanceof DurableSessionLogError)) return undefined;
			const mapped = mapLogError(error);
			const next = new NopalSessionFaultBridge({
				path: defaultSessionSocketPath(binding, options.runtimeRoot),
				fault: {
					binding: { ...binding },
					code: mapped.code,
					retryable: false,
					message: mapped.message,
				},
			});
			try {
				await next.start();
				if (stopped || permanentlyClosed) {
					await next.close();
					return undefined;
				}
				bridge = next;
				activeBinding = { ...binding };
				return next.endpoint();
			} catch {
				return undefined;
			}
		}
		const next = (options.bridgeFactory ?? ((bridgeOptions) => new NopalSessionBridge(bridgeOptions)))({
			path: defaultSessionSocketPath(binding, options.runtimeRoot),
			engine: nextEngine,
		});
		try {
			const nextActivityProducer = new SessionActivityProducer({
				binding,
				existingEvents: nextEngine.log.events(),
				publish: (input) => nextEngine.publishActivity(input),
			});
			await next.start();
			if (stopped || permanentlyClosed) {
				await next.close();
				return undefined;
			}
			bridge = next;
			engine = nextEngine;
			activityProducer = nextActivityProducer;
			activeBinding = { ...binding };
			rememberEngine();
			return next.endpoint();
		} catch {
			await next.close().catch(() => undefined);
			return undefined;
		}
	};
	const bind = (binding: SessionBinding, history?: SessionHistorySource): Promise<SessionEndpoint | undefined> =>
		serialize(() => bindNow({ ...binding }, history));
	const refresh = (cwd: string): Promise<SessionEndpoint | undefined> =>
		serialize(async () => {
			if (stopped || permanentlyClosed) return undefined;
			const binding = await resolveNopalSessionBinding(exec, {
				cwd,
				paneId: options.paneId ?? process.env.TMUX_PANE,
			});
			if (stopped || permanentlyClosed) return undefined;
			return binding ? bindNow(binding, activeHistory) : bridge?.endpoint();
		});

	registerSessionActivityHooks(pi, {
		producer: () => activityProducer,
		commandId: () => engine?.activeCommandId(),
		onError(error, context) {
			const detail = error instanceof Error ? error.message : String(error);
			const message = boundActivityText(
				`Nopal Session activity was not recorded: ${detail}`,
				MAX_ACTIVITY_FAILURE_BYTES,
			).text;
			if (options.activityDiagnostic) {
				options.activityDiagnostic(message);
			} else if (context.hasUI) {
				context.ui.notify(message, "warning");
			} else {
				console.error(message);
			}
		},
	});

	pi.on("session_start", async (_event, ctx) => {
		const generation = ++lifecycleGeneration;
		await serialize(async () => {
			if (permanentlyClosed || generation !== lifecycleGeneration) return undefined;
			stopped = false;
			activeHistory = historyFromContext(ctx) ?? activeHistory;
			await closeActive();
			if (permanentlyClosed || generation !== lifecycleGeneration) return undefined;
			const binding = await resolveNopalSessionBinding(exec, {
				cwd: ctx.cwd,
				paneId: options.paneId ?? process.env.TMUX_PANE,
			});
			if (permanentlyClosed || generation !== lifecycleGeneration) return undefined;
			return binding ? bindNow(binding, activeHistory) : undefined;
		});
	});

	pi.on("session_tree", async (_event, ctx) => {
		await serialize(async () => {
			if (!activeBinding || stopped || permanentlyClosed) return undefined;
			activeHistory = historyFromContext(ctx) ?? activeHistory;
			const binding = { ...activeBinding };
			return bindNow(binding, activeHistory, true);
		});
	});
	pi.on("input", (event) => {
		try {
			engine?.observeInput(event.text, event.source);
		} catch {
			// Persistence failure leaves the prior durable prefix authoritative.
		}
		return { action: "continue" };
	});
	pi.on("agent_start", () => {
		engine?.observeAgentStart();
	});
	pi.on("message_end", (event) => {
		if (event.message.role === "assistant") engine?.observeAssistant(event.message);
	});
	pi.on("agent_end", () => {
		try {
			engine?.observeAgentEnd();
		} catch {
			// Persistence failure leaves the prior durable prefix authoritative.
		}
	});
	pi.on("session_shutdown", async () => {
		stopped = true;
		lifecycleGeneration += 1;
		await serialize(async () => {
			stopped = true;
			await closeActive();
		});
	});

	return {
		endpoint: () => bridge?.endpoint(),
		bind,
		refresh,
		async close() {
			permanentlyClosed = true;
			lifecycleGeneration += 1;
			await serialize(async () => {
				stopped = true;
				await closeActive();
			});
		},
	};
}
