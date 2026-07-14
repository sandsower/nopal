import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
	observeAfkRun,
	submitAfkRun,
	type ExecFn,
	type ObservationHandle,
	type RunObservationEnvelope,
} from "./nopal-cli.js";

export const AFK_RESULT_KIND = "nopal.afk_result/v1" as const;
export const MAX_ACCUMULATED_EVENTS = 500;
export const MAX_ACCUMULATED_EVENT_BYTES = 2 * 1024 * 1024;
const DEFAULT_RESULT_TIMEOUT_MS = 60_000;
const DEFAULT_POLL_INTERVAL_MS = 1_000;
const EVENT_CURSOR_PREFIX = "rondo.core/v1:";
const EVENT_CURSOR_PATTERN = /^rondo\.core\/v1:[0-9]{1,20}$/u;
const EVENT_CURSOR_SCHEMA_PATTERN = "^rondo\\.core/v1:[0-9]{1,20}$";

export type AfkResultOutcome = "observed" | "settled" | "timeout" | "aborted" | "budget_exhausted" | "cursor_stalled" | "error";

export type AfkResultReport = {
	kind: typeof AFK_RESULT_KIND;
	ok: boolean;
	outcome: AfkResultOutcome;
	handle: ObservationHandle;
	status: string | null;
	last_event: unknown;
	evidence_pointers: unknown[];
	event_cursor: string | null;
	events: unknown[];
	next_event_cursor: string | null;
	has_more: boolean;
	settled: boolean;
	polls: number;
	diagnostics: string[];
};

export type AfkResultParams = {
	repoId: string;
	plotId: string;
	runId: string;
	eventCursor?: string;
	block?: boolean;
	timeoutMs?: number;
	pollIntervalMs?: number;
	cwd: string;
};

export type PollRuntime = {
	now?: () => number;
	sleep?: (milliseconds: number, signal?: AbortSignal) => Promise<boolean>;
	maxEvents?: number;
	maxEventBytes?: number;
};

type SchemaFactory = {
	Object: (properties: Record<string, unknown>) => any;
	String: (options?: Record<string, unknown>) => any;
	Optional: (schema: unknown) => any;
	Boolean: (options?: Record<string, unknown>) => any;
	Integer: (options?: Record<string, unknown>) => any;
};

function abortableSleep(milliseconds: number, signal?: AbortSignal): Promise<boolean> {
	if (signal?.aborted) return Promise.resolve(false);
	return new Promise((resolve) => {
		let finished = false;
		const finish = (completed: boolean) => {
			if (finished) return;
			finished = true;
			clearTimeout(timer);
			signal?.removeEventListener("abort", abort);
			resolve(completed);
		};
		const abort = () => finish(false);
		const timer = setTimeout(() => finish(true), milliseconds);
		signal?.addEventListener("abort", abort, { once: true });
	});
}
function serializedBytes(value: unknown): number {
	try {
		return Buffer.byteLength(JSON.stringify(value), "utf8");
	} catch {
		return Number.POSITIVE_INFINITY;
	}
}

function validIdentifier(value: string, maxBytes: number): boolean {
	return value.length > 0 && value.trim() === value && !/[\u0000-\u001f\u007f-\u009f]/u.test(value) && Buffer.byteLength(value, "utf8") <= maxBytes;
}

function safeIdentifierEcho(value: string, maxBytes: number): string {
	return validIdentifier(value, maxBytes) ? value : "-";
}

function invalidInput(params: AfkResultParams): string | undefined {
	if (!validIdentifier(params.repoId, 512)) return "AFK result repository identifier is invalid";
	if (!validIdentifier(params.plotId, 512)) return "AFK result Plot identifier is invalid";
	if (!validIdentifier(params.runId, 4_096)) return "AFK result run identifier is invalid";
	if (params.eventCursor !== undefined && !EVENT_CURSOR_PATTERN.test(params.eventCursor)) return "AFK result event cursor is invalid";
	return undefined;
}

function cursorOffset(cursor: string): bigint {
	return BigInt(cursor.slice(EVENT_CURSOR_PREFIX.length));
}

function reportFrom(
	params: AfkResultParams,
	latest: RunObservationEnvelope | undefined,
	events: unknown[],
	nextEventCursor: string | null,
	polls: number,
	outcome: AfkResultOutcome,
	diagnostic?: string,
): AfkResultReport {
	const diagnostics = latest?.diagnostics.slice() ?? [];
	if (diagnostic) diagnostics.push(diagnostic);
	return {
		kind: AFK_RESULT_KIND,
		ok: outcome !== "error" && outcome !== "cursor_stalled",
		outcome,
		handle: latest?.handle ?? {
			repo_id: safeIdentifierEcho(params.repoId, 512),
			plot_id: safeIdentifierEcho(params.plotId, 512),
			run_id: safeIdentifierEcho(params.runId, 4_096),
		},
		status: latest?.status ?? null,
		last_event: latest?.last_event ?? null,
		evidence_pointers: latest?.evidence_pointers ?? [],
		event_cursor: latest?.event_cursor ?? null,
		events,
		next_event_cursor: nextEventCursor,
		has_more: outcome === "budget_exhausted" || (latest?.has_more ?? false),
		settled: outcome === "settled",
		polls,
		diagnostics,
	};
}

/** Poll Rondo only through one-shot Nopal observations. All state is local to this call. */
export async function readAfkResult(exec: ExecFn, params: AfkResultParams, signal?: AbortSignal, runtime: PollRuntime = {}): Promise<AfkResultReport> {
	const now = runtime.now ?? Date.now;
	const sleep = runtime.sleep ?? abortableSleep;
	const block = params.block ?? false;
	const timeoutMs = Math.max(1, Math.floor(params.timeoutMs ?? DEFAULT_RESULT_TIMEOUT_MS));
	const pollIntervalMs = Math.max(1, Math.floor(params.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS));
	const maxEvents = runtime.maxEvents ?? MAX_ACCUMULATED_EVENTS;
	const maxEventBytes = runtime.maxEventBytes ?? MAX_ACCUMULATED_EVENT_BYTES;
	const deadline = now() + timeoutMs;
	let cursor = params.eventCursor ?? null;
	let latest: RunObservationEnvelope | undefined;
	let polls = 0;
	const events: unknown[] = [];
	let eventBytes = 2;
	const inputError = invalidInput(params);
	if (inputError) return reportFrom(params, undefined, events, null, 0, "error", inputError);

	while (true) {
		if (signal?.aborted) return reportFrom(params, latest, events, cursor, polls, "aborted", "AFK observation was aborted; the Rondo run was not cancelled");
		if (block && polls > 0 && now() >= deadline) return reportFrom(params, latest, events, cursor, polls, "timeout", "AFK observation timed out before the run settled");

		const remaining = block ? Math.max(1, deadline - now()) : timeoutMs;
		const observation = await observeAfkRun(exec, {
			repoId: params.repoId,
			plotId: params.plotId,
			runId: params.runId,
			eventCursor: cursor ?? undefined,
			cwd: params.cwd,
			timeoutMs: remaining,
			signal,
		});
		polls += 1;

		if (signal?.aborted) return reportFrom(params, latest, events, cursor, polls, "aborted", "AFK observation was aborted; the Rondo run was not cancelled");
		if (!observation.ok) {
			if (block && now() >= deadline) return reportFrom(params, latest, events, cursor, polls, "timeout", "AFK observation timed out before the run settled");
			const failed = observation.envelope;
			if (failed) latest = failed;
			return reportFrom(params, latest, events, cursor, polls, "error", observation.error);
		}

		const page = observation.envelope;
		latest = page;
		const requestedCursor = cursor ?? `${EVENT_CURSOR_PREFIX}0`;
		const expectedNextOffset = cursorOffset(requestedCursor) + BigInt(page.events.length);
		if (cursorOffset(page.next_event_cursor) !== expectedNextOffset) {
			return reportFrom(params, latest, events, cursor, polls, "cursor_stalled", "Rondo event pagination cursor did not match the returned event count");
		}
		if ((page.has_more || page.events.length > 0) && page.next_event_cursor === requestedCursor) {
			return reportFrom(params, latest, events, cursor, polls, "cursor_stalled", "Rondo event pagination did not advance its cursor");
		}

		let pageBytes = eventBytes;
		let pageEventCount = events.length;
		for (const event of page.events) {
			pageBytes += serializedBytes(event) + (pageEventCount > 0 ? 1 : 0);
			pageEventCount += 1;
		}
		if (events.length + page.events.length > maxEvents || pageBytes > maxEventBytes) {
			return reportFrom(params, latest, events, cursor, polls, "budget_exhausted", "AFK result event accumulation reached its per-call budget");
		}

		for (const event of page.events) {
			eventBytes += serializedBytes(event) + (events.length > 0 ? 1 : 0);
			events.push(event);
		}
		cursor = page.next_event_cursor;

		if (page.settled) return reportFrom(params, latest, events, cursor, polls, "settled");
		if (!block) return reportFrom(params, latest, events, cursor, polls, "observed");
		if (page.has_more) continue;
		if (now() >= deadline) return reportFrom(params, latest, events, cursor, polls, "timeout", "AFK observation timed out before the run settled");

		const completedSleep = await sleep(Math.min(pollIntervalMs, Math.max(1, deadline - now())), signal);
		if (!completedSleep || signal?.aborted) return reportFrom(params, latest, events, cursor, polls, "aborted", "AFK observation was aborted; the Rondo run was not cancelled");
	}
}

function toolResult(value: unknown, isError = false) {
	return { content: [{ type: "text" as const, text: JSON.stringify(value, null, 2) }], details: value, isError };
}

export function registerAfkTools(pi: ExtensionAPI, exec: ExecFn, Type: SchemaFactory): void {
	pi.registerTool({
		name: "nopal_afk_start",
		label: "Start Nopal AFK Run",
		description: "Submit one approved per-slice manifest to Rondo Core through Nopal.",
		promptSnippet: "Start an approved Nopal AFK run",
		promptGuidelines: ["Pass only an approved exported per-slice manifest. Nopal evaluates readiness and policy before Rondo Core submission."],
		parameters: Type.Object({
			manifestPath: Type.String({ minLength: 1, description: "Approved per-slice manifest path inside the current repository" }),
			plotId: Type.String({ minLength: 1, maxLength: 512, description: "Established Plot identity that owns this execution" }),
		}),
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			const result = await submitAfkRun(exec, { manifestPath: params.manifestPath, plotId: params.plotId, cwd: ctx.cwd, signal });
			if (result.envelope) return toolResult(result.envelope, !result.ok);
			return toolResult({ kind: "nopal.afk_start_error/v1", ok: false, error: result.error }, true);
		},
	});

	pi.registerTool({
		name: "nopal_afk_result",
		label: "Read Nopal AFK Result",
		description: "Observe a Rondo-owned AFK run through bounded one-shot Nopal polls. Observation never cancels execution.",
		promptSnippet: "Poll or wait for a Nopal AFK run result",
		promptGuidelines: ["Use block=false for one observation. Use block=true to drain available pages and wait locally for a terminal result."],
		parameters: Type.Object({
			repoId: Type.String({ minLength: 1, maxLength: 512, description: "Opaque repository identifier from nopal_afk_start" }),
			plotId: Type.String({ minLength: 1, maxLength: 512, description: "Plot identifier from nopal_afk_start" }),
			runId: Type.String({ minLength: 1, description: "Opaque Rondo run identifier from nopal_afk_start" }),
			eventCursor: Type.Optional(Type.String({
				minLength: EVENT_CURSOR_PREFIX.length + 1,
				maxLength: EVENT_CURSOR_PREFIX.length + 20,
				pattern: EVENT_CURSOR_SCHEMA_PATTERN,
				description: "Opaque cursor from a prior start or result",
			})),
			block: Type.Optional(Type.Boolean({ description: "Wait locally until settled, timeout, abort, or accumulation budget" })),
			timeoutMs: Type.Optional(Type.Integer({ minimum: 1, maximum: 3_600_000 })),
			pollIntervalMs: Type.Optional(Type.Integer({ minimum: 1, maximum: 60_000 })),
		}),
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			const report = await readAfkResult(exec, { ...params, cwd: ctx.cwd }, signal);
			return toolResult(report, !report.ok);
		},
	});
}
