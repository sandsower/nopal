/**
 * Subprocess helpers for the Nopal CLI reads/writes used by the Nopal
 * session extension: `nopal --json workflow show`, `nopal --json ledger
 * pointer`, the `nopal ledger init/event/finalize` babysit recording calls,
 * and the one-shot Rondo Core submit/observe operator commands.
 *
 * Pure module: the actual process spawn is injected as `exec` (matching the
 * shape of `pi.exec` from `@earendil-works/pi-coding-agent`, same seam as
 * `extensions/policy-gate/nopal-cli.ts`), so tests can fake it without
 * spawning Pi or the real Nopal binary.
 *
 * Config and checkpoint reads fail safe: any problem reaching or parsing the
 * Nopal binary (missing binary, nonzero exit, unparseable JSON, `ok: false`)
 * returns `undefined` so callers can fall back to safe defaults. Ledger
 * writes (init/event/finalize) are best-effort: failures are reported back
 * as `{ ok: false, error }` so callers can notify the UI and continue.
 */

export type ExecResult = {
	stdout: string;
	stderr: string;
	code: number;
	killed?: boolean;
};

export type ExecOptions = {
	cwd?: string;
	timeout?: number;
	signal?: AbortSignal;
};

/** Matches the call shape of `pi.exec` so `pi.exec` can be passed directly. */
export type ExecFn = (command: string, args: string[], options?: ExecOptions) => Promise<ExecResult>;

const DEFAULT_TIMEOUT_MS = 10_000;
const NOPAL_BIN = "nopal";

export const RUN_SUBMIT_KIND = "nopal.run_submit/v1" as const;
export const RUN_OBSERVATION_KIND = "nopal.run_observation/v1" as const;
export const PLOT_ESTABLISHMENT_KIND = "nopal.plot_establishment/v1" as const;

export type AfkRunHandle = {
	service_id: string;
	repo_id: string;
	plot_id: string;
	run_id: string;
	status: string;
	event_cursor: string;
};

export type RunSubmitEnvelope = {
	kind: typeof RUN_SUBMIT_KIND;
	ok: boolean;
	submitted: boolean;
	deduplicated: boolean;
	manifest_path: string | null;
	manifest_sha256: string | null;
	decision: string | null;
	placement: string | null;
	handle: AfkRunHandle | null;
	diagnostics: string[];
};

export type ObservationHandle = {
	repo_id: string;
	plot_id: string;
	run_id: string;
};

export type RunObservationEnvelope = {
	kind: typeof RUN_OBSERVATION_KIND;
	ok: boolean;
	handle: ObservationHandle;
	status: string | null;
	last_event: unknown;
	evidence_pointers: unknown[];
	event_cursor: string | null;
	events: unknown[];
	next_event_cursor: string | null;
	has_more: boolean;
	settled: boolean;
	diagnostics: string[];
};

export type AfkCliResult<T> = { ok: true; envelope: T } | { ok: false; error: string; envelope?: T };

export type PlotEstablishmentEnvelope = {
	kind: typeof PLOT_ESTABLISHMENT_KIND;
	ok: boolean;
	outcome: "established" | "extended" | "unchanged" | null;
	plot: { plot_id: string; selected_session_id?: string | null } | null;
	diagnostics: unknown[];
};

export type PlotEstablishmentResult =
	| { ok: true; envelope: PlotEstablishmentEnvelope }
	| { ok: false; error: string; envelope?: PlotEstablishmentEnvelope };

export type PlotSessionProtocolEndpoint = {
	kind: string;
	transport: "unix";
	address: string;
	state: string;
};

export type NopalSessionBinding = {
	plotId: string;
	sessionId: string;
};

export type SubmitAfkParams = {
	manifestPath: string;
	plotId: string;
	cwd: string;
	timeoutMs?: number;
	signal?: AbortSignal;
};

export type ObserveAfkParams = {
	repoId: string;
	plotId: string;
	runId: string;
	eventCursor?: string;
	cwd: string;
	timeoutMs?: number;
	signal?: AbortSignal;
};

function isObject(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isNonemptySafeString(value: unknown): value is string {
	return typeof value === "string" && value.length > 0 && value.trim() === value && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function isEventCursor(value: unknown): value is string {
	return typeof value === "string" && /^rondo\.core\/v1:[0-9]{1,20}$/u.test(value);
}

function isNullableString(value: unknown): value is string | null {
	return value === null || typeof value === "string";
}

export function parsePlotEstablishmentEnvelope(value: unknown): PlotEstablishmentEnvelope | undefined {
	if (!isObject(value) || value.kind !== PLOT_ESTABLISHMENT_KIND || typeof value.ok !== "boolean") return undefined;
	if (!Array.isArray(value.diagnostics)) return undefined;
	const outcome = value.outcome;
	if (outcome !== null && outcome !== "established" && outcome !== "extended" && outcome !== "unchanged") return undefined;
	let plot: { plot_id: string; selected_session_id?: string | null } | null = null;
	if (value.plot !== null) {
		if (!isObject(value.plot) || !isNonemptySafeString(value.plot.plot_id)) return undefined;
		const selected = value.plot.selected_session_id;
		if (selected !== undefined && selected !== null && !isNonemptySafeString(selected)) return undefined;
		plot = {
			plot_id: value.plot.plot_id,
			...(selected === undefined ? {} : { selected_session_id: selected as string | null }),
		};
	}
	if (value.ok !== (outcome !== null && plot !== null)) return undefined;
	return { kind: PLOT_ESTABLISHMENT_KIND, ok: value.ok, outcome, plot, diagnostics: value.diagnostics };
}

export async function establishPlot(
	exec: ExecFn,
	params: { event: string; cwd: string; protocol?: PlotSessionProtocolEndpoint },
): Promise<PlotEstablishmentResult> {
	const args = ["plot", "establish", "--event", params.event, "--workspace", params.cwd];
	if (params.protocol) {
		args.push(
			"--protocol-kind",
			params.protocol.kind,
			"--protocol-address",
			params.protocol.address,
			"--protocol-state",
			params.protocol.state,
		);
	}
	const outcome = await runNopalJson(
		exec,
		args,
		params.cwd,
		DEFAULT_TIMEOUT_MS,
	);
	const envelope = parsePlotEstablishmentEnvelope(outcome.json);
	if (!envelope) return { ok: false, error: outcome.error ?? "Nopal Plot Establishment returned an invalid envelope" };
	if (!outcome.ok || !envelope.ok) {
		return { ok: false, error: outcome.error ?? "Nopal Plot Establishment was rejected", envelope };
	}
	return { ok: true, envelope };
}

/** Resolve the Core-owned Plot/Session identity already stamped on this Pi pane. */
export async function resolveNopalSessionBinding(
	exec: ExecFn,
	params: { cwd: string; paneId?: string },
): Promise<NopalSessionBinding | undefined> {
	if (!params.paneId || !/^%[0-9]+$/u.test(params.paneId)) return undefined;
	const readOption = async (option: string): Promise<string | undefined> => {
		try {
			const result = await exec("tmux", ["show-options", "-qv", "-t", params.paneId!, option], {
				cwd: params.cwd,
				timeout: DEFAULT_TIMEOUT_MS,
			});
			if (result.code !== 0) return undefined;
			const value = result.stdout.replace(/\n$/u, "");
			return isNonemptySafeString(value) && Buffer.byteLength(value, "utf8") <= 512 ? value : undefined;
		} catch {
			return undefined;
		}
	};
	const plotId = await readOption("@nopal_plot");
	const sessionId = await readOption("@nopal_plot_session");
	return plotId && sessionId ? { plotId, sessionId } : undefined;
}

function parseRunHandle(value: unknown): AfkRunHandle | undefined {
	if (!isObject(value)) return undefined;
	if (!isNonemptySafeString(value.service_id) || Buffer.byteLength(value.service_id, "utf8") > 512) return undefined;
	if (!isNonemptySafeString(value.repo_id) || Buffer.byteLength(value.repo_id, "utf8") > 512) return undefined;
	if (!isNonemptySafeString(value.plot_id) || Buffer.byteLength(value.plot_id, "utf8") > 512) return undefined;
	if (!isNonemptySafeString(value.run_id) || !isNonemptySafeString(value.status) || !isEventCursor(value.event_cursor)) return undefined;
	return {
		service_id: value.service_id,
		repo_id: value.repo_id,
		plot_id: value.plot_id,
		run_id: value.run_id,
		status: value.status,
		event_cursor: value.event_cursor,
	};
}

/** Strictly parse a `nopal.run_submit/v1` CLI envelope. */
export function parseRunSubmitEnvelope(value: unknown): RunSubmitEnvelope | undefined {
	if (!isObject(value) || value.kind !== RUN_SUBMIT_KIND) return undefined;
	if (typeof value.ok !== "boolean" || typeof value.submitted !== "boolean" || typeof value.deduplicated !== "boolean") return undefined;
	if (!isNullableString(value.manifest_path) || !isNullableString(value.manifest_sha256)) return undefined;
	if (!isNullableString(value.decision) || !isNullableString(value.placement) || !isStringArray(value.diagnostics)) return undefined;
	if (value.manifest_sha256 !== null && !/^[0-9a-f]{64}$/u.test(value.manifest_sha256)) return undefined;
	const handle = value.handle === null ? null : parseRunHandle(value.handle);
	if (handle === undefined) return undefined;
	if (value.ok) {
		if (!value.submitted || handle === null || value.manifest_path === null || value.manifest_sha256 === null || value.decision === null || value.placement === null) return undefined;
		if (value.manifest_path.length === 0 || value.decision !== "allow" || value.placement === "blocked" || !isNonemptySafeString(value.placement)) return undefined;
	} else if (value.submitted || handle !== null || value.deduplicated) {
		return undefined;
	}
	return {
		kind: RUN_SUBMIT_KIND,
		ok: value.ok,
		submitted: value.submitted,
		deduplicated: value.deduplicated,
		manifest_path: value.manifest_path,
		manifest_sha256: value.manifest_sha256,
		decision: value.decision,
		placement: value.placement,
		handle,
		diagnostics: value.diagnostics,
	};
}

const TERMINAL_RUN_STATUSES = new Set(["completed", "failed", "terminated", "paused"]);

/** Strictly parse a `nopal.run_observation/v1` CLI envelope and verify its handle echo. */
export function parseRunObservationEnvelope(value: unknown, expected?: ObservationHandle): RunObservationEnvelope | undefined {
	if (!isObject(value) || value.kind !== RUN_OBSERVATION_KIND || !isObject(value.handle)) return undefined;
	if (!isNonemptySafeString(value.handle.repo_id) || Buffer.byteLength(value.handle.repo_id, "utf8") > 512) return undefined;
	if (!isNonemptySafeString(value.handle.plot_id) || Buffer.byteLength(value.handle.plot_id, "utf8") > 512) return undefined;
	if (!isNonemptySafeString(value.handle.run_id)) return undefined;
	if (expected && (value.handle.repo_id !== expected.repo_id || value.handle.plot_id !== expected.plot_id || value.handle.run_id !== expected.run_id)) return undefined;
	if (typeof value.ok !== "boolean" || !isNullableString(value.status) || !isNullableString(value.event_cursor)) return undefined;
	if (!isNullableString(value.next_event_cursor) || !Array.isArray(value.evidence_pointers) || !Array.isArray(value.events)) return undefined;
	if (!("last_event" in value) || typeof value.has_more !== "boolean" || typeof value.settled !== "boolean" || !isStringArray(value.diagnostics)) return undefined;
	if (value.status !== null && !isNonemptySafeString(value.status)) return undefined;
	if (value.event_cursor !== null && !isNonemptySafeString(value.event_cursor)) return undefined;
	if (value.next_event_cursor !== null && !isNonemptySafeString(value.next_event_cursor)) return undefined;
	if (value.event_cursor !== null && !isEventCursor(value.event_cursor)) return undefined;
	if (value.next_event_cursor !== null && !isEventCursor(value.next_event_cursor)) return undefined;
	if (value.ok && (value.status === null || value.event_cursor === null || value.next_event_cursor === null)) return undefined;
	if (!value.ok && (value.has_more || value.settled)) return undefined;
	if (value.settled && (value.has_more || value.status === null || !TERMINAL_RUN_STATUSES.has(value.status))) return undefined;
	if (value.ok && value.status !== null && value.settled !== (TERMINAL_RUN_STATUSES.has(value.status) && !value.has_more)) return undefined;
	return {
		kind: RUN_OBSERVATION_KIND,
		ok: value.ok,
		handle: { repo_id: value.handle.repo_id, plot_id: value.handle.plot_id, run_id: value.handle.run_id },
		status: value.status,
		last_event: value.last_event,
		evidence_pointers: value.evidence_pointers,
		event_cursor: value.event_cursor,
		events: value.events,
		next_event_cursor: value.next_event_cursor,
		has_more: value.has_more,
		settled: value.settled,
		diagnostics: value.diagnostics,
	};
}

function safeAfkFailure(action: "submission" | "observation", reason: "execution" | "output" | "envelope" | "status"): string {
	const messages = {
		execution: `Nopal AFK ${action} could not execute the nopal binary`,
		output: `Nopal AFK ${action} returned unparseable output`,
		envelope: `Nopal AFK ${action} returned an invalid envelope`,
		status: `Nopal AFK ${action} exit status did not match its envelope`,
	};
	return messages[reason];
}

async function invokeAfkEnvelope<T>(
	exec: ExecFn,
	action: "submission" | "observation",
	args: string[],
	cwd: string,
	timeoutMs: number | undefined,
	parse: (value: unknown) => T | undefined,
	signal?: AbortSignal,
): Promise<AfkCliResult<T>> {
	let result: ExecResult;
	try {
		result = await exec(NOPAL_BIN, ["--json", ...args], { cwd, ...(timeoutMs !== undefined ? { timeout: timeoutMs } : {}), ...(signal ? { signal } : {}) });
	} catch {
		return { ok: false, error: safeAfkFailure(action, "execution") };
	}
	if (result.killed) return { ok: false, error: safeAfkFailure(action, "execution") };
	let value: unknown;
	try {
		value = JSON.parse(result.stdout);
	} catch {
		return { ok: false, error: safeAfkFailure(action, "output") };
	}
	const envelope = parse(value);
	if (!envelope) return { ok: false, error: safeAfkFailure(action, "envelope") };
	const envelopeOk = (envelope as { ok?: unknown }).ok === true;
	if ((result.code === 0) !== envelopeOk) return { ok: false, error: safeAfkFailure(action, "status") };
	if (!envelopeOk) return { ok: false, error: `Nopal AFK ${action} was rejected`, envelope };
	return { ok: true, envelope };
}

/** Invoke only the Plot-scoped `nopal --json run submit` command. */
export function submitAfkRun(exec: ExecFn, params: SubmitAfkParams): Promise<AfkCliResult<RunSubmitEnvelope>> {
	return invokeAfkEnvelope(exec, "submission", ["run", "submit", "--manifest", params.manifestPath, "--plot-id", params.plotId], params.cwd, params.timeoutMs, parseRunSubmitEnvelope, params.signal);
}

/** Invoke one bounded `nopal --json run observe` call. */
export function observeAfkRun(exec: ExecFn, params: ObserveAfkParams): Promise<AfkCliResult<RunObservationEnvelope>> {
	const args = ["run", "observe", "--repo-id", params.repoId, "--plot-id", params.plotId, "--run-id", params.runId];
	if (params.eventCursor !== undefined) args.push("--cursor", params.eventCursor);
	return invokeAfkEnvelope(
		exec,
		"observation",
		args,
		params.cwd,
		params.timeoutMs,
		(value) => parseRunObservationEnvelope(value, { repo_id: params.repoId, plot_id: params.plotId, run_id: params.runId }),
		params.signal,
	);
}

async function runNopalJson(exec: ExecFn, args: string[], cwd: string | undefined, timeoutMs: number): Promise<{ ok: boolean; json?: unknown; error?: string }> {
	let result: ExecResult;
	try {
		result = await exec(NOPAL_BIN, ["--json", ...args], { cwd, timeout: timeoutMs });
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return { ok: false, error: `nopal binary could not be executed (${message})` };
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(result.stdout);
	} catch {
		return { ok: false, error: `nopal ${args.join(" ")} produced unparseable output` };
	}
	if (result.code !== 0) {
		return { ok: false, json: parsed, error: `nopal ${args.join(" ")} exited with code ${result.code}` };
	}
	return { ok: true, json: parsed };
}

// ---------------------------------------------------------------------------
// workflow show
// ---------------------------------------------------------------------------

export type WorkflowShowResult = {
	handoff: { auto: boolean; events: string[]; exclude: string[] };
	babysit: { tokenBudget: number | null };
	establishment: { events: string[] };
};

/** Parse a `nopal.workflow.show/v1` envelope. Returns undefined on any shape mismatch. */
export function parseWorkflowShowEnvelope(json: unknown): WorkflowShowResult | undefined {
	if (!json || typeof json !== "object") return undefined;
	const envelope = json as { ok?: unknown; handoff?: unknown; babysit?: unknown; establishment?: unknown };
	if (envelope.ok !== true) return undefined;
	const handoff = envelope.handoff as { auto?: unknown; events?: unknown; exclude?: unknown } | undefined;
	const babysit = envelope.babysit as { token_budget?: unknown } | undefined;
	const establishment = envelope.establishment as { events?: unknown } | undefined;
	if (!handoff || typeof handoff.auto !== "boolean") return undefined;
	const events = isStringArray(handoff.events) ? handoff.events : [];
	const exclude = isStringArray(handoff.exclude) ? handoff.exclude : [];
	const rawBudget = babysit?.token_budget;
	const tokenBudget = typeof rawBudget === "number" && Number.isFinite(rawBudget) && rawBudget > 0 ? rawBudget : null;
	const establishmentEvents = isStringArray(establishment?.events) ? establishment.events : [];
	return {
		handoff: { auto: handoff.auto, events, exclude },
		babysit: { tokenBudget },
		establishment: { events: establishmentEvents },
	};
}

/** Run `nopal --json workflow show`. Returns undefined when the CLI could not be consulted. */
export async function fetchWorkflowShow(exec: ExecFn, cwd: string): Promise<WorkflowShowResult | undefined> {
	const outcome = await runNopalJson(exec, ["workflow", "show"], cwd, DEFAULT_TIMEOUT_MS);
	if (!outcome.ok) return undefined;
	return parseWorkflowShowEnvelope(outcome.json);
}

// ---------------------------------------------------------------------------
// ledger pointer
// ---------------------------------------------------------------------------

export type PointerEntry = {
	event: string;
	path: string;
	ticket?: { id?: string; title?: string };
	branch?: string;
	source_skill?: string;
	written_at?: string;
};

function parsePointerEntry(value: unknown): PointerEntry | undefined {
	if (!isObject(value)) return undefined;
	if (typeof value.event !== "string" || typeof value.path !== "string") return undefined;
	const entry: PointerEntry = { event: value.event, path: value.path };
	if (isObject(value.ticket)) {
		entry.ticket = {
			id: typeof value.ticket.id === "string" ? value.ticket.id : undefined,
			title: typeof value.ticket.title === "string" ? value.ticket.title : undefined,
		};
	}
	if (typeof value.branch === "string") entry.branch = value.branch;
	if (typeof value.source_skill === "string") entry.source_skill = value.source_skill;
	if (typeof value.written_at === "string") entry.written_at = value.written_at;
	return entry;
}

export type LedgerPointerResult = {
	/** Relative path of the pointer file that was read, or null when neither location exists. */
	source: string | null;
	entries: PointerEntry[];
};

/** Parse a `nopal.run_ledger.pointer/v1` envelope. Returns undefined on any shape mismatch. */
export function parseLedgerPointerEnvelope(json: unknown): LedgerPointerResult | undefined {
	if (!json || typeof json !== "object") return undefined;
	const envelope = json as { ok?: unknown; source?: unknown; entries?: unknown };
	if (envelope.ok !== true || !Array.isArray(envelope.entries)) return undefined;
	const entries = envelope.entries.map(parsePointerEntry).filter((entry): entry is PointerEntry => entry !== undefined);
	const source = typeof envelope.source === "string" ? envelope.source : null;
	return { source, entries };
}

/** Run `nopal --json ledger pointer`. Returns undefined when the CLI could not be consulted. */
export async function fetchLedgerPointer(exec: ExecFn, cwd: string): Promise<LedgerPointerResult | undefined> {
	const outcome = await runNopalJson(exec, ["ledger", "pointer"], cwd, DEFAULT_TIMEOUT_MS);
	if (!outcome.ok) return undefined;
	return parseLedgerPointerEnvelope(outcome.json);
}

// ---------------------------------------------------------------------------
// ledger init / event / finalize (babysit recording)
// ---------------------------------------------------------------------------

export type LedgerWriteResult = { ok: true } | { ok: false; error: string };

export type LedgerInitResult = { ok: true; runId: string } | { ok: false; error: string };

export type LedgerInitParams = {
	skill: string;
	flow?: string;
	ticketId?: string;
	ticketTitle?: string;
	cwd?: string;
};

function extractDiagnosticMessages(value: unknown): string[] {
	if (!Array.isArray(value)) return [];
	return value
		.map((entry) => (entry && typeof entry === "object" ? (entry as { message?: unknown }).message : undefined))
		.filter((message): message is string => typeof message === "string");
}

function describeFailure(action: string, outcome: { ok: boolean; json?: unknown; error?: string }): string {
	const diagnostics = extractDiagnosticMessages((outcome.json as { diagnostics?: unknown } | undefined)?.diagnostics);
	if (diagnostics.length > 0) return `${action} failed: ${diagnostics.join("; ")}`;
	return outcome.error ?? `${action} failed`;
}

/** Run `nopal ledger init --skill <skill> [--flow <flow>] [--ticket-id <id>] [--ticket-title <title>]`. */
export async function ledgerInit(exec: ExecFn, params: LedgerInitParams): Promise<LedgerInitResult> {
	const args = ["ledger", "init", "--skill", params.skill];
	if (params.flow) args.push("--flow", params.flow);
	if (params.ticketId) args.push("--ticket-id", params.ticketId);
	if (params.ticketTitle) args.push("--ticket-title", params.ticketTitle);
	const outcome = await runNopalJson(exec, args, params.cwd, DEFAULT_TIMEOUT_MS);
	if (!outcome.ok) return { ok: false, error: describeFailure("ledger init", outcome) };
	const runId = (outcome.json as { run_id?: unknown } | undefined)?.run_id;
	if (typeof runId !== "string" || runId.length === 0) {
		return { ok: false, error: "ledger init did not return a run_id" };
	}
	return { ok: true, runId };
}

export type LedgerEventParams = {
	runId: string;
	type: string;
	summary?: string;
	flow?: string;
	cwd?: string;
};

/** Run `nopal ledger event --run-id <id> --type <type> [--summary <summary>] [--flow <flow>]`. */
export async function ledgerEvent(exec: ExecFn, params: LedgerEventParams): Promise<LedgerWriteResult> {
	const args = ["ledger", "event", "--run-id", params.runId, "--type", params.type];
	if (params.summary) args.push("--summary", params.summary);
	if (params.flow) args.push("--flow", params.flow);
	const outcome = await runNopalJson(exec, args, params.cwd, DEFAULT_TIMEOUT_MS);
	if (!outcome.ok) return { ok: false, error: describeFailure("ledger event", outcome) };
	return { ok: true };
}

export type LedgerFinalizeStatus = "completed" | "interrupted" | "failed";

export type LedgerFinalizeParams = {
	runId: string;
	status: LedgerFinalizeStatus;
	flow?: string;
	cwd?: string;
};

/** Run `nopal ledger finalize --run-id <id> --status <status> [--flow <flow>]`. */
export async function ledgerFinalize(exec: ExecFn, params: LedgerFinalizeParams): Promise<LedgerWriteResult> {
	const args = ["ledger", "finalize", "--run-id", params.runId, "--status", params.status];
	if (params.flow) args.push("--flow", params.flow);
	const outcome = await runNopalJson(exec, args, params.cwd, DEFAULT_TIMEOUT_MS);
	if (!outcome.ok) return { ok: false, error: describeFailure("ledger finalize", outcome) };
	return { ok: true };
}
