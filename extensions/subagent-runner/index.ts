import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { writeAtomicJson } from "./atomic-json.js";
import { attachPostExitStdioGuard } from "./post-exit-stdio-guard.js";
import { buildPiSubprocessInvocationFromPromptFile, SUBAGENT_CHILD_ENV } from "./subagent-pi-spawn.js";
import { buildTmuxKillInvocation, createTmuxWorkspaceSpec, type TmuxWorkspaceSpec } from "./subagent-workspace-tmux.js";
import { reduceSubagentUiState, renderRunningSubagentsWidget, renderSubagentDetailLines, renderSubagentListLines, type SubagentUiState } from "./subagent-workspace-ui.js";

export { writeAtomicJson } from "./atomic-json.js";
export { attachPostExitStdioGuard } from "./post-exit-stdio-guard.js";
export { buildPiSubprocessInvocationFromPromptFile, SUBAGENT_CHILD_ENV } from "./subagent-pi-spawn.js";
export { createTmuxWorkspaceSpec, buildTmuxKillInvocation } from "./subagent-workspace-tmux.js";
export { reduceSubagentUiState, renderRunningSubagentsWidget, renderSubagentDetailLines, renderSubagentListLines } from "./subagent-workspace-ui.js";

export type SubagentStatus = "success" | "error" | "timeout" | "killed";
export type SubagentMode = "headless" | "terminal";

export type SubagentRunInput = {
	prompt: string;
	label?: string;
	cwd?: string;
	timeoutSeconds?: number;
	agentDir?: string;
	now?: Date;
	onStart?: (record: SubagentRunRecord) => void;
	terminal?: boolean;
	allowMutations?: boolean;
	promptPolicy?: "compat" | "background";
};

export type SubagentProcessLifecycle = {
	onStarted?: (info: { pid?: number; invocation?: SubagentInvocation }) => void;
};

export type SubagentInvocation = {
	command: string;
	args: string[];
	env?: Record<string, string | undefined>;
};

export type SubagentProcessRequest = {
	prompt: string;
	promptPath: string;
	cwd: string;
	label: string;
	artifactDir: string;
	transcriptPath: string;
	stderrPath: string;
	timeoutSeconds: number;
	tmuxSpec?: TmuxWorkspaceSpec;
};

export type SubagentProcessResult = {
	exitCode: number;
	stdout: string;
	stderr: string;
	result?: string;
	transcript?: string;
	invocation?: SubagentInvocation;
	pid?: number;
	terminalCleanupStatus?: SubagentTerminalMetadata["cleanupStatus"];
};

export type SubagentRunResult = {
	status: SubagentStatus;
	label: string;
	cwd: string;
	prompt: string;
	result: string;
	artifactDir: string;
	promptPath: string;
	resultPath: string;
	transcriptPath: string;
	stderrPath: string;
	metadataPath: string;
	startedAt: string;
	completedAt: string;
	exitCode?: number;
	timedOut: boolean;
	error?: string;
};

export type SubagentProcessRunner = (
	request: SubagentProcessRequest,
	signal?: AbortSignal,
	lifecycle?: SubagentProcessLifecycle,
) => Promise<SubagentProcessResult>;

export type SubagentCommandRunner = (
	invocation: SubagentInvocation,
	cwd: string,
	signal?: AbortSignal,
) => Promise<{ exitCode: number; stdout: string; stderr: string }>;

export type SubagentTerminalMetadata = {
	backend: "tmux";
	socketName: string;
	sessionName: string;
	attachCommand: string;
	cleanupStatus: "none" | "active" | "cleaned" | "kept" | "failed";
};

export type SubagentRunRecord = {
	id: string;
	label: string;
	mode: SubagentMode;
	cwd: string;
	promptPreview: string;
	artifactDir: string;
	promptPath: string;
	resultPath: string;
	transcriptPath: string;
	stderrPath: string;
	metadataPath: string;
	status: SubagentStatus | "running";
	pid?: number;
	startedAt: string;
	completedAt?: string;
	exitCode?: number;
	error?: string;
	terminal?: SubagentTerminalMetadata;
};

export type SubagentRegistry = {
	runs: SubagentRunRecord[];
};

export type SubagentStartResult = {
	status: "started" | "failed_to_start";
	id: string;
	label: string;
	mode: SubagentMode;
	cwd: string;
	artifactDir: string;
	promptPath: string;
	resultPath: string;
	transcriptPath: string;
	stderrPath: string;
	metadataPath: string;
	attachCommand?: string;
	error?: string;
};

export type StartedSubagentWorkspace = {
	record: SubagentRunRecord;
	started: Promise<SubagentStartResult>;
	completion: Promise<SubagentRunResult>;
};

export type SubagentResultRetrievalStatus = "success" | "not_ready" | "timeout" | "not_found";

export type SubagentResultRead = {
	retrieval_status: SubagentResultRetrievalStatus;
	run?: {
		id: string;
		label: string;
		mode: SubagentMode;
		status: SubagentRunRecord["status"];
		cwd: string;
		result?: string;
		resultTruncated?: boolean;
		transcriptTail?: string;
		stderrTail?: string;
		error?: string;
		exitCode?: number;
		artifactDir: string;
		promptPath: string;
		resultPath: string;
		transcriptPath: string;
		stderrPath: string;
		metadataPath: string;
		attachCommand?: string;
	};
};

type PreparedSubagentWorkspace = {
	input: SubagentRunInput;
	agentDir: string;
	label: string;
	cwd: string;
	prompt: string;
	delegatedPrompt: string;
	timeoutSeconds: number;
	startedAt: string;
	paths: ReturnType<typeof createSubagentArtifactPaths>;
	controller: AbortController;
	record: SubagentRunRecord;
	tmuxSpec?: TmuxWorkspaceSpec;
};

type TimerApi = {
	setTimeout: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
	clearTimeout: (timer: ReturnType<typeof setTimeout>) => void;
};

type IntervalTimerApi = {
	setInterval: (fn: () => void, ms: number) => ReturnType<typeof setInterval>;
	clearInterval: (timer: ReturnType<typeof setInterval>) => void;
};

export type ParsedSubagentCommand =
	| { ok: true; prompt: string; terminal: boolean; label?: string; timeoutSeconds?: number }
	| { ok: false; error: string };

const DEFAULT_TIMEOUT_SECONDS = 300;
const MIN_TIMEOUT_SECONDS = 5;
const MAX_TIMEOUT_SECONDS = 3600;
const SUBAGENT_DIR = "subagents";
const SUBAGENT_REGISTRY_FILE = "runs.json";
const MAX_RECENT_SUBAGENT_RUNS = 50;
const MAX_RUNNING_SUBAGENTS = 4;
const DEFAULT_RESULT_TIMEOUT_MS = 30_000;
const DEFAULT_RESULT_MAX_CHARS = 20_000;
const MAX_RESULT_MAX_CHARS = 100_000;
const RESULT_TAIL_LINES = 40;
const RESULT_TAIL_CHARS = 8_000;
const READ_ONLY_PREFIX = [
	"You are a focused subagent running in an isolated pi context.",
	"Prefer read-only analysis. Do not edit files or mutate external systems unless the task explicitly asks you to.",
	"Return concise findings for the parent session.",
].join("\n");

const BACKGROUND_READ_ONLY_PREFIX = [
	"You are a focused background subagent running in an isolated pi context.",
	"Default policy: read-only. Do not edit files, run destructive commands, or mutate external systems unless this prompt explicitly says mutations are allowed.",
	"Return a concise final report for the parent session using this structure:",
	"",
	"## Result",
	"One-paragraph answer.",
	"",
	"## Evidence",
	"- Files, commands, or artifacts inspected.",
	"",
	"## Issues / Blockers",
	"- Anything that prevented completion.",
	"",
	"## Next Step",
	"Recommended parent action.",
].join("\n");

const activeSubagentControllers = new Map<string, AbortController>();
const killedSubagentRuns = new Set<string>();

const SubagentRunParams = Type.Object({
	prompt: Type.String({ description: "Focused task prompt for the subagent" }),
	label: Type.Optional(Type.String({ description: "Short label used for artifact paths" })),
	cwd: Type.Optional(Type.String({ description: "Working directory for the subagent. Defaults to the current cwd." })),
	timeoutSeconds: Type.Optional(Type.Number({ description: "Timeout in seconds, clamped between 5 and 3600. Default: 300." })),
	terminal: Type.Optional(Type.Boolean({ description: "Run in a managed tmux-backed terminal workspace. Default: false." })),
});

const SubagentStartParams = Type.Object({
	prompt: Type.String({ description: "Focused task prompt for the background subagent" }),
	label: Type.Optional(Type.String({ description: "Short label used for artifact paths" })),
	cwd: Type.Optional(Type.String({ description: "Working directory for the subagent. Defaults to the current cwd." })),
	timeoutSeconds: Type.Optional(Type.Number({ description: "Timeout in seconds, clamped between 5 and 3600. Default: 300." })),
	terminal: Type.Optional(Type.Boolean({ description: "Run in a managed tmux-backed terminal workspace. Default: false." })),
	allowMutations: Type.Optional(Type.Boolean({ description: "Allow file edits or other mutations in the delegated task. Default: false." })),
});

const SubagentResultParams = Type.Object({
	id: Type.String({ description: "Subagent workspace id" }),
	block: Type.Optional(Type.Boolean({ description: "Wait for completion before returning. Default: false." })),
	timeoutMs: Type.Optional(Type.Number({ description: "Max wait time in milliseconds when block=true. Default: 30000." })),
	maxChars: Type.Optional(Type.Number({ description: "Max result characters to return. Default: 20000." })),
});

const SubagentListParams = Type.Object({
	status: Type.Optional(Type.String({ description: "Filter: running, completed, attention, or all. Default: all." })),
	limit: Type.Optional(Type.Number({ description: "Maximum runs to return. Default: 20." })),
});

const SubagentKillParams = Type.Object({
	id: Type.String({ description: "Subagent workspace id to kill" }),
	reason: Type.String({ description: "Reason for killing this workspace" }),
});

export function sanitizeSubagentLabel(value: string | undefined): string {
	const clean = (value ?? "subagent")
		.toLowerCase()
		.replace(/[^a-z0-9._-]+/g, "-")
		.replace(/^-+|-+$/g, "")
		.slice(0, 80);
	return /[a-z0-9]/.test(clean) ? clean : "subagent";
}

export function normalizeTimeoutSeconds(value: number | undefined): number {
	if (!Number.isFinite(value)) return DEFAULT_TIMEOUT_SECONDS;
	return Math.max(MIN_TIMEOUT_SECONDS, Math.min(MAX_TIMEOUT_SECONDS, Math.floor(value as number)));
}

export function parseSubagentCommandArgs(args: string): ParsedSubagentCommand {
	const tokens = tokenizeCommandArgs(args);
	let terminal = false;
	let label: string | undefined;
	let timeoutSeconds: number | undefined;
	const promptTokens: string[] = [];
	let parsingFlags = true;

	for (let i = 0; i < tokens.length; i++) {
		const token = tokens[i]!;
		if (parsingFlags && token === "--") {
			parsingFlags = false;
			continue;
		}
		if (parsingFlags && token === "--terminal") {
			terminal = true;
			continue;
		}
		if (parsingFlags && token === "--label") {
			const value = tokens[++i];
			if (!value) return { ok: false, error: "Missing value for --label" };
			label = value;
			continue;
		}
		if (parsingFlags && token === "--timeout") {
			const value = tokens[++i];
			const parsed = Number(value);
			if (!value || !Number.isFinite(parsed)) return { ok: false, error: "Invalid value for --timeout" };
			timeoutSeconds = parsed;
			continue;
		}
		promptTokens.push(token);
	}

	const prompt = promptTokens.join(" ").trim();
	if (!prompt) return { ok: false, error: "Usage: /subagent [--terminal] [--label <label>] [--timeout <seconds>] <focused prompt>" };
	return { ok: true, prompt, terminal, ...(label ? { label } : {}), ...(timeoutSeconds !== undefined ? { timeoutSeconds } : {}) };
}

export function buildDelegatedSubagentPrompt(prompt: string, policy: "compat" | "background" = "compat", allowMutations = false): string {
	if (policy === "background") {
		const mutationNote = allowMutations
			? "\n\nMutations are explicitly allowed for this task. Keep changes minimal, evidence-bound, and report exactly what changed."
			: "";
		return `${BACKGROUND_READ_ONLY_PREFIX}${mutationNote}\n\nTask: ${prompt}\n`;
	}
	return `${READ_ONLY_PREFIX}\n\nTask: ${prompt}\n`;
}

function tokenizeCommandArgs(input: string): string[] {
	const tokens: string[] = [];
	let current = "";
	let quote: "'" | '"' | undefined;
	for (let i = 0; i < input.length; i++) {
		const char = input[i]!;
		if (quote) {
			if (char === quote) quote = undefined;
			else current += char;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			continue;
		}
		if (/\s/.test(char)) {
			if (current) {
				tokens.push(current);
				current = "";
			}
			continue;
		}
		current += char;
	}
	if (current) tokens.push(current);
	return tokens;
}

export function createSubagentArtifactPaths(agentDir: string, label: string, now = new Date()) {
	const timestamp = now.toISOString().replace(/[:.]/g, "-").toLowerCase();
	const safeLabel = sanitizeSubagentLabel(label);
	const artifactDir = join(agentDir, SUBAGENT_DIR, `${timestamp}-${safeLabel}`);
	return {
		artifactDir,
		promptPath: join(artifactDir, "prompt.md"),
		resultPath: join(artifactDir, "result.md"),
		transcriptPath: join(artifactDir, "transcript.jsonl"),
		stderrPath: join(artifactDir, "stderr.log"),
		metadataPath: join(artifactDir, "metadata.json"),
	};
}

function subagentRegistryPath(agentDir: string): string {
	return join(agentDir, SUBAGENT_DIR, SUBAGENT_REGISTRY_FILE);
}

export function loadSubagentRegistry(agentDir: string): SubagentRegistry {
	const path = subagentRegistryPath(agentDir);
	if (!existsSync(path)) return { runs: [] };
	try {
		const parsed = JSON.parse(readFileSync(path, "utf-8")) as Partial<SubagentRegistry>;
		return { runs: Array.isArray(parsed.runs) ? parsed.runs.map(normalizeSubagentRunRecord).filter(Boolean) as SubagentRunRecord[] : [] };
	} catch {
		return { runs: [] };
	}
}

function saveSubagentRegistry(agentDir: string, registry: SubagentRegistry): void {
	const path = subagentRegistryPath(agentDir);
	mkdirSync(dirname(path), { recursive: true });
	const running = registry.runs.filter((run) => run.status === "running");
	const recentTerminal = registry.runs
		.filter((run) => run.status !== "running")
		.sort((a, b) => (b.completedAt ?? b.startedAt).localeCompare(a.completedAt ?? a.startedAt))
		.slice(0, MAX_RECENT_SUBAGENT_RUNS);
	writeAtomicJson(path, { runs: [...running, ...recentTerminal] });
}

function normalizeSubagentRunRecord(value: unknown): SubagentRunRecord | undefined {
	if (!value || typeof value !== "object") return undefined;
	const record = value as Partial<SubagentRunRecord>;
	if (typeof record.id !== "string" || typeof record.artifactDir !== "string") return undefined;
	const status = isSubagentStatus(record.status) || record.status === "running" ? record.status : "error";
	return {
		id: record.id,
		label: typeof record.label === "string" ? sanitizeSubagentLabel(record.label) : "subagent",
		mode: record.mode === "terminal" ? "terminal" : "headless",
		cwd: typeof record.cwd === "string" ? record.cwd : "",
		promptPreview: typeof record.promptPreview === "string" ? record.promptPreview : "",
		artifactDir: record.artifactDir,
		promptPath: typeof record.promptPath === "string" ? record.promptPath : join(record.artifactDir, "prompt.md"),
		resultPath: typeof record.resultPath === "string" ? record.resultPath : join(record.artifactDir, "result.md"),
		transcriptPath: typeof record.transcriptPath === "string" ? record.transcriptPath : join(record.artifactDir, "transcript.jsonl"),
		stderrPath: typeof record.stderrPath === "string" ? record.stderrPath : join(record.artifactDir, "stderr.log"),
		metadataPath: typeof record.metadataPath === "string" ? record.metadataPath : join(record.artifactDir, "metadata.json"),
		status,
		pid: record.pid,
		startedAt: typeof record.startedAt === "string" ? record.startedAt : new Date(0).toISOString(),
		completedAt: record.completedAt,
		exitCode: record.exitCode,
		error: record.error,
		terminal: record.terminal,
	};
}

function isSubagentStatus(value: unknown): value is SubagentStatus {
	return value === "success" || value === "error" || value === "timeout" || value === "killed";
}

function writeSubagentMetadata(path: string, metadata: Record<string, unknown>): void {
	writeAtomicJson(path, metadata);
}

export function createSubagentRunRecord(input: {
	agentDir: string;
	label: string;
	cwd: string;
	prompt: string;
	paths: ReturnType<typeof createSubagentArtifactPaths>;
	startedAt: string;
	pid?: number;
	mode?: SubagentMode;
	terminal?: SubagentTerminalMetadata;
}): SubagentRunRecord {
	const artifactName = basename(input.paths.artifactDir);
	return {
		id: artifactName,
		label: sanitizeSubagentLabel(input.label),
		mode: input.mode ?? "headless",
		cwd: input.cwd,
		promptPreview: input.prompt.replace(/\s+/g, " ").trim().slice(0, 160),
		artifactDir: input.paths.artifactDir,
		promptPath: input.paths.promptPath,
		resultPath: input.paths.resultPath,
		transcriptPath: input.paths.transcriptPath,
		stderrPath: input.paths.stderrPath,
		metadataPath: input.paths.metadataPath,
		status: "running",
		pid: input.pid,
		startedAt: input.startedAt,
		terminal: input.terminal,
	};
}

export function upsertSubagentRunRecord(agentDir: string, record: SubagentRunRecord): void {
	const registry = loadSubagentRegistry(agentDir);
	const index = registry.runs.findIndex((run) => run.id === record.id);
	if (index === -1) registry.runs.unshift(record);
	else registry.runs[index] = { ...registry.runs[index], ...record };
	saveSubagentRegistry(agentDir, registry);
}

export function completeSubagentRunRecord(
	agentDir: string,
	id: string,
	patch: Pick<SubagentRunRecord, "status" | "completedAt"> & Partial<Pick<SubagentRunRecord, "exitCode" | "error" | "terminal">>,
): void {
	const registry = loadSubagentRegistry(agentDir);
	const run = registry.runs.find((candidate) => candidate.id === id);
	if (!run) return;
	Object.assign(run, patch);
	saveSubagentRegistry(agentDir, registry);
}

export function dismissSubagentRunRecord(agentDir: string, id: string): void {
	const registry = loadSubagentRegistry(agentDir);
	saveSubagentRegistry(agentDir, { runs: registry.runs.filter((run) => run.id !== id) });
}

export function dismissCompletedSubagentRunRecords(agentDir: string): number {
	const registry = loadSubagentRegistry(agentDir);
	const runs = registry.runs.filter((run) => run.status === "running");
	const dismissed = registry.runs.length - runs.length;
	saveSubagentRegistry(agentDir, { runs });
	return dismissed;
}

export async function cleanupWorkspaceTerminal(
	agentDir: string,
	id: string,
	commandRunner: SubagentCommandRunner = runInvocation,
): Promise<{ ok: true } | { ok: false; error: string }> {
	const run = loadSubagentRegistry(agentDir).runs.find((candidate) => candidate.id === id);
	if (!run) return { ok: false, error: "Workspace not found" };
	if (!run.terminal) return { ok: false, error: "Workspace has no terminal session" };
	const killed = await commandRunner(buildTmuxKillInvocation(run.terminal.sessionName), run.cwd);
	const cleanupStatus = killed.exitCode === 0 ? "cleaned" : "failed";
	completeSubagentRunRecord(agentDir, id, {
		status: run.status,
		completedAt: run.completedAt ?? new Date().toISOString(),
		terminal: { ...run.terminal, cleanupStatus },
	});
	return killed.exitCode === 0 ? { ok: true } : { ok: false, error: killed.stderr || "Failed to cleanup terminal session" };
}

export async function killWorkspace(
	agentDir: string,
	id: string,
	reasonOrCommandRunner?: string | SubagentCommandRunner,
	maybeCommandRunner?: SubagentCommandRunner,
): Promise<{ ok: true } | { ok: false; error: string }> {
	const reason = typeof reasonOrCommandRunner === "string" && reasonOrCommandRunner.trim() ? reasonOrCommandRunner.trim() : "Killed by user";
	const commandRunner = typeof reasonOrCommandRunner === "function" ? reasonOrCommandRunner : maybeCommandRunner ?? runInvocation;
	const controller = activeSubagentControllers.get(id);
	const run = loadSubagentRegistry(agentDir).runs.find((candidate) => candidate.id === id);
	if (!run) return { ok: false, error: "Workspace not found" };
	if (run.status !== "running") return { ok: false, error: "Workspace is not running" };
	if (run.terminal) {
		const killed = await commandRunner(buildTmuxKillInvocation(run.terminal.sessionName), run.cwd);
		if (killed.exitCode !== 0) return { ok: false, error: killed.stderr || "Failed to kill terminal session" };
		killedSubagentRuns.add(id);
		completeSubagentRunRecord(agentDir, id, {
			status: "killed",
			completedAt: new Date().toISOString(),
			error: reason,
			terminal: { ...run.terminal, cleanupStatus: "cleaned" },
		});
		writeKillReason(run.metadataPath, reason);
		controller?.abort();
		return { ok: true };
	}
	if (!controller) return { ok: false, error: "Workspace is not active in this pi process" };
	killedSubagentRuns.add(id);
	completeSubagentRunRecord(agentDir, id, { status: "killed", completedAt: new Date().toISOString(), error: reason });
	writeKillReason(run.metadataPath, reason);
	controller.abort();
	return { ok: true };
}

function writeKillReason(metadataPath: string, reason: string): void {
	try {
		const metadata = existsSync(metadataPath) ? JSON.parse(readFileSync(metadataPath, "utf-8")) as Record<string, unknown> : {};
		writeSubagentMetadata(metadataPath, { ...metadata, killReason: reason, error: reason });
	} catch {
		writeSubagentMetadata(metadataPath, { killReason: reason, error: reason });
	}
}

export function countRunningSubagents(agentDir: string): number {
	return loadSubagentRegistry(agentDir).runs.filter((run) => run.status === "running").length;
}

export function checkSubagentCapacity(agentDir: string): void {
	const running = loadSubagentRegistry(agentDir).runs.filter((run) => run.status === "running");
	if (running.length < MAX_RUNNING_SUBAGENTS) return;
	throw new Error(`Cannot start subagent: ${running.length} running workspaces already active.\n\n${formatRunningCapacitySummary(running)}`);
}

export function formatRunningCapacitySummary(runs: SubagentRunRecord[]): string {
	const lines = ["Running subagents:"];
	for (const run of runs.sort(compareSubagentRuns)) {
		lines.push(`- ${run.id} ${run.label} ${run.promptPreview || "(empty)"}`);
		lines.push(`  artifacts: ${run.artifactDir}`);
	}
	return lines.join("\n");
}

export function listSubagentRuns(agentDir: string): SubagentRunRecord[] {
	return loadSubagentRegistry(agentDir).runs.sort(compareSubagentRuns);
}

function compareSubagentRuns(a: SubagentRunRecord, b: SubagentRunRecord): number {
	const group = (run: SubagentRunRecord) => {
		if (run.status === "running") return 0;
		if (run.status === "error" || run.status === "timeout") return 1;
		if (run.status === "killed") return 2;
		return 3;
	};
	const groupDelta = group(a) - group(b);
	if (groupDelta !== 0) return groupDelta;
	return (b.completedAt ?? b.startedAt).localeCompare(a.completedAt ?? a.startedAt);
}

export function formatSubagentRuns(runs: SubagentRunRecord[], now = Date.now()): string {
	if (runs.length === 0) return "No subagent runs found.";
	const lines = ["Subagents", ""];
	for (const run of runs) {
		const duration = formatRunDuration(run, now);
		lines.push(`- ${run.status === "running" ? "●" : "○"} ${run.label} [${run.status}] ${duration}`);
		lines.push(`  id: ${run.id}`);
		lines.push(`  task: ${run.promptPreview || "(empty)"}`);
		lines.push(`  Artifacts: ${run.artifactDir}`);
		if (run.error) lines.push(`  error: ${run.error}`);
	}
	return lines.join("\n");
}

function formatRunDuration(run: SubagentRunRecord, now: number): string {
	const start = Date.parse(run.startedAt);
	const end = run.completedAt ? Date.parse(run.completedAt) : now;
	if (!Number.isFinite(start) || !Number.isFinite(end)) return "";
	const seconds = Math.max(0, Math.round((end - start) / 1000));
	return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m${seconds % 60}s`;
}

export function filterSubagentRuns(runs: SubagentRunRecord[], status: string = "all", limit = 20): SubagentRunRecord[] {
	const filtered = runs.filter((run) => {
		if (status === "running") return run.status === "running";
		if (status === "completed") return run.status !== "running";
		if (status === "attention") return run.status === "error" || run.status === "timeout" || run.status === "killed" || run.terminal?.cleanupStatus === "kept";
		return true;
	});
	return filtered.slice(0, Math.max(1, Math.min(100, Math.floor(limit))));
}

export function subagentNotificationLevel(status: SubagentRunResult["status"]): "success" | "error" | "warning" {
	if (status === "success") return "success";
	if (status === "error") return "error";
	return "warning";
}

export function subagentToolResultIsError(status: SubagentRunResult["status"]): boolean {
	return status !== "success";
}

function truncateNotificationText(text: string, maxChars = 1_200): string {
	const compact = text.trim();
	if (compact.length <= maxChars) return compact;
	return `${compact.slice(0, Math.max(0, maxChars - 24)).trimEnd()}\n… truncated; open /subagents`;
}

export function formatSubagentStartNotification(record: SubagentRunRecord): string {
	const lines = [`Subagent ${record.label} started.`, "", `id: ${record.id}`, "Output: /subagents or subagent_result"];
	if (record.mode === "terminal") lines.push("Terminal attach: open /subagents");
	return lines.join("\n");
}

export function formatSubagentCompletionNotification(result: SubagentRunResult): string {
	const verb = result.status === "success" ? "completed" : result.status === "timeout" ? "timed out" : result.status === "killed" ? "was killed" : "failed";
	const id = basename(result.artifactDir);
	const lines = [`Subagent ${result.label} ${verb}.`, "", `id: ${id}`];
	if (result.result.trim()) lines.push("", "Result preview:", truncateNotificationText(result.result));
	if (result.error && result.status === "error") lines.push("", `Error: ${truncateNotificationText(result.error, 500)}`);
	lines.push("", "Full output: /subagents or subagent_result");
	return lines.join("\n");
}

export function formatSubagentReport(result: SubagentRunResult): string {
	const verb = result.status === "success" ? "completed" : result.status === "timeout" ? "timed out" : result.status === "killed" ? "was killed" : "failed";
	const lines = [`Subagent ${result.label} ${verb}.`, ""];
	if (result.result.trim()) {
		lines.push("Result:");
		lines.push(result.result.trim());
		lines.push("");
	}
	if (result.error && result.status === "error") {
		lines.push(`Error: ${result.error}`);
		lines.push("");
	} else if (result.error && result.status === "timeout" && !result.result.includes(result.error)) {
		lines.push(`Timed out: ${result.error.replace(/^Subagent\s+/i, "")}`);
		lines.push("");
	} else if (result.error && result.status === "killed") {
		lines.push(`Stopped: ${result.error}`);
		lines.push("");
	}
	lines.push("Artifacts:");
	lines.push(`- result: ${result.resultPath}`);
	lines.push(`- transcript: ${result.transcriptPath}`);
	lines.push(`- metadata: ${result.metadataPath}`);
	return lines.join("\n").trimEnd();
}

export function startSubagentWorkspace(
	input: SubagentRunInput,
	runner: SubagentProcessRunner = defaultSubagentProcessRunner,
	timers: TimerApi = { setTimeout, clearTimeout },
): StartedSubagentWorkspace {
	const prepared = prepareSubagentWorkspace(input);
	let timedOut = false;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let startedResolved = false;
	let resolveStarted!: (result: SubagentStartResult) => void;
	const started = new Promise<SubagentStartResult>((resolve) => { resolveStarted = resolve; });
	const startResult = (status: SubagentStartResult["status"], error?: string): SubagentStartResult => ({
		status,
		id: prepared.record.id,
		label: prepared.label,
		mode: prepared.record.mode,
		cwd: prepared.cwd,
		...prepared.paths,
		...(prepared.tmuxSpec?.terminal.attachCommand ? { attachCommand: prepared.tmuxSpec.terminal.attachCommand } : {}),
		...(error ? { error } : {}),
	});
	const resolveStartedOnce = (result: SubagentStartResult) => {
		if (startedResolved) return;
		startedResolved = true;
		resolveStarted(result);
	};

	const completion = (async (): Promise<SubagentRunResult> => {
		let processResult: SubagentProcessResult | undefined;
		let error: string | undefined;
		try {
			const effectiveRunner = input.terminal && runner === defaultSubagentProcessRunner ? defaultTmuxSubagentProcessRunner : runner;
			const promise = effectiveRunner({ prompt: prepared.delegatedPrompt, promptPath: prepared.paths.promptPath, cwd: prepared.cwd, label: prepared.label, artifactDir: prepared.paths.artifactDir, transcriptPath: prepared.paths.transcriptPath, stderrPath: prepared.paths.stderrPath, timeoutSeconds: prepared.timeoutSeconds, tmuxSpec: prepared.tmuxSpec }, prepared.controller.signal, {
				onStarted: () => resolveStartedOnce(startResult("started")),
			});
			timer = timers.setTimeout(() => {
				timedOut = true;
				prepared.controller.abort();
			}, prepared.timeoutSeconds * 1000);
			processResult = await promise;
			if (!startedResolved && processResult.exitCode !== 0) resolveStartedOnce(startResult("failed_to_start", processResult.stderr || `Subagent exited with code ${processResult.exitCode}`));
			else resolveStartedOnce(startResult("started"));
		} catch (caught) {
			error = caught instanceof Error ? caught.message : String(caught);
			if (!startedResolved) resolveStartedOnce(startResult("failed_to_start", error));
		} finally {
			if (timer) timers.clearTimeout(timer);
		}
		return finalizeSubagentWorkspace(prepared, { processResult, error, timedOut });
	})();

	return { record: prepared.record, started, completion };
}

export async function runSubagentTask(
	input: SubagentRunInput,
	runner: SubagentProcessRunner = defaultSubagentProcessRunner,
	timers: TimerApi = { setTimeout, clearTimeout },
): Promise<SubagentRunResult> {
	const workspace = startSubagentWorkspace(input, runner, timers);
	await workspace.started;
	return workspace.completion;
}

function prepareSubagentWorkspace(input: SubagentRunInput): PreparedSubagentWorkspace {
	const prompt = input.prompt.trim();
	if (!prompt) throw new Error("Subagent prompt is required");

	const label = sanitizeSubagentLabel(input.label ?? "subagent");
	const cwd = input.cwd ?? process.cwd();
	const agentDir = input.agentDir ?? getAgentDir();
	const timeoutSeconds = normalizeTimeoutSeconds(input.timeoutSeconds);
	checkSubagentCapacity(agentDir);
	const startedAtDate = input.now ?? new Date();
	const startedAt = startedAtDate.toISOString();
	const paths = createSubagentArtifactPaths(agentDir, label, startedAtDate);
	mkdirSync(paths.artifactDir, { recursive: true });

	const delegatedPrompt = buildDelegatedSubagentPrompt(prompt, input.promptPolicy ?? "compat", input.allowMutations);
	writeFileSync(paths.promptPath, delegatedPrompt, "utf-8");
	writeFileSync(paths.resultPath, "", "utf-8");
	writeFileSync(paths.transcriptPath, "", "utf-8");
	writeFileSync(paths.stderrPath, "", "utf-8");

	const controller = new AbortController();
	const mode: SubagentMode = input.terminal ? "terminal" : "headless";
	const tmuxSpec = input.terminal ? createTmuxWorkspaceSpec({ id: basename(paths.artifactDir), cwd, prompt: delegatedPrompt, paths }) : undefined;
	const record = createSubagentRunRecord({ agentDir, label, cwd, prompt, paths, startedAt, mode, terminal: tmuxSpec?.terminal });
	upsertSubagentRunRecord(agentDir, record);
	activeSubagentControllers.set(record.id, controller);
	writeSubagentMetadata(paths.metadataPath, {
		label,
		mode,
		cwd,
		startedAt,
		status: "running",
		artifactDir: paths.artifactDir,
		promptSource: "artifact-file",
		promptPath: paths.promptPath,
		originalPromptPreview: record.promptPreview,
		terminal: tmuxSpec?.terminal,
	});
	input.onStart?.(record);
	return { input, agentDir, label, cwd, prompt, delegatedPrompt, timeoutSeconds, startedAt, paths, controller, record, tmuxSpec };
}

function finalizeSubagentWorkspace(
	prepared: PreparedSubagentWorkspace,
	state: { processResult?: SubagentProcessResult; error?: string; timedOut: boolean },
): SubagentRunResult {
	activeSubagentControllers.delete(prepared.record.id);
	const exitCode = state.processResult?.exitCode;
	let error = state.error;
	const status: SubagentStatus = killedSubagentRuns.has(prepared.record.id) ? "killed" : state.timedOut ? "timeout" : exitCode === 0 && !error ? "success" : "error";
	if (status === "killed") killedSubagentRuns.delete(prepared.record.id);
	if (!error && status === "error") error = `Subagent exited with code ${exitCode ?? "unknown"}`;
	if (!error && status === "timeout") error = `Subagent timed out after ${prepared.timeoutSeconds} seconds`;

	const rawStdout = state.processResult?.stdout ?? "";
	const transcript = state.processResult?.transcript ?? rawStdout;
	let resultText = status === "timeout"
		? extractFinalTextFromJsonLines(transcript || rawStdout) ?? ""
		: state.processResult?.result ?? extractFinalTextFromJsonLines(rawStdout) ?? rawStdout.trim();
	if (status === "timeout" && !resultText.trim()) resultText = formatTimeoutPartialResult(error ?? `Subagent timed out after ${prepared.timeoutSeconds} seconds`, transcript || rawStdout);
	const completedAt = new Date().toISOString();

	writeFileSync(prepared.paths.resultPath, `${resultText.trim()}\n`, "utf-8");
	writeFileSync(prepared.paths.transcriptPath, transcript, "utf-8");
	writeFileSync(prepared.paths.stderrPath, state.processResult?.stderr ?? "", "utf-8");

	const terminal = prepared.tmuxSpec?.terminal ? { ...prepared.tmuxSpec.terminal, cleanupStatus: terminalCleanupStatus(status, state.processResult) } : undefined;
	writeSubagentMetadata(prepared.paths.metadataPath, {
		label: prepared.label,
		mode: prepared.record.mode,
		cwd: prepared.cwd,
		startedAt: prepared.startedAt,
		completedAt,
		status,
		artifactDir: prepared.paths.artifactDir,
		promptSource: "artifact-file",
		promptPath: prepared.paths.promptPath,
		originalPromptPreview: prepared.record.promptPreview,
		terminal,
		command: state.processResult?.invocation?.command,
		args: state.processResult?.invocation?.args,
		exitCode,
		timedOut: state.timedOut,
		error,
	});
	completeSubagentRunRecord(prepared.agentDir, prepared.record.id, { status, completedAt, exitCode, error, ...(terminal ? { terminal } : {}) });

	return {
		status,
		label: prepared.label,
		cwd: prepared.cwd,
		prompt: prepared.prompt,
		result: resultText.trim(),
		...prepared.paths,
		startedAt: prepared.startedAt,
		completedAt,
		exitCode,
		timedOut: state.timedOut,
		error,
	};
}

export async function readSubagentResult(
	agentDir: string,
	id: string,
	options: { block?: boolean; timeoutMs?: number; maxChars?: number; signal?: AbortSignal } = {},
): Promise<SubagentResultRead> {
	const timeoutMs = normalizeResultTimeoutMs(options.timeoutMs);
	const start = Date.now();
	while (true) {
		const run = findSubagentRun(agentDir, id);
		if (!run) return { retrieval_status: "not_found" };
		if (run.status !== "running") return { retrieval_status: "success", run: subagentResultFromRecord(run, options.maxChars) };
		if (!options.block) return { retrieval_status: "not_ready", run: subagentResultFromRecord(run, options.maxChars) };
		if (Date.now() - start >= timeoutMs) return { retrieval_status: "timeout", run: subagentResultFromRecord(run, options.maxChars) };
		if (options.signal?.aborted) return { retrieval_status: "timeout", run: subagentResultFromRecord(run, options.maxChars) };
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
}

export function findSubagentRun(agentDir: string, id: string): SubagentRunRecord | undefined {
	if (!isSafeSubagentId(id)) return undefined;
	const registryRun = loadSubagentRegistry(agentDir).runs.find((candidate) => candidate.id === id);
	if (registryRun) return registryRun;
	const artifactDir = join(agentDir, SUBAGENT_DIR, id);
	const metadataPath = join(artifactDir, "metadata.json");
	if (!existsSync(metadataPath)) return undefined;
	try {
		const metadata = JSON.parse(readFileSync(metadataPath, "utf-8")) as Record<string, unknown>;
		const status = metadata.status === "running" || isSubagentStatus(metadata.status) ? metadata.status : "error";
		return {
			id,
			label: sanitizeSubagentLabel(typeof metadata.label === "string" ? metadata.label : id),
			mode: metadata.mode === "terminal" ? "terminal" : "headless",
			cwd: typeof metadata.cwd === "string" ? metadata.cwd : "",
			promptPreview: typeof metadata.originalPromptPreview === "string" ? metadata.originalPromptPreview : typeof metadata.promptPreview === "string" ? metadata.promptPreview : "",
			artifactDir,
			promptPath: join(artifactDir, "prompt.md"),
			resultPath: join(artifactDir, "result.md"),
			transcriptPath: join(artifactDir, "transcript.jsonl"),
			stderrPath: join(artifactDir, "stderr.log"),
			metadataPath,
			status,
			startedAt: typeof metadata.startedAt === "string" ? metadata.startedAt : new Date(0).toISOString(),
			completedAt: typeof metadata.completedAt === "string" ? metadata.completedAt : undefined,
			exitCode: typeof metadata.exitCode === "number" ? metadata.exitCode : undefined,
			error: typeof metadata.error === "string" ? metadata.error : undefined,
			terminal: metadata.terminal as SubagentTerminalMetadata | undefined,
		};
	} catch {
		return undefined;
	}
}

function isSafeSubagentId(id: string): boolean {
	return Boolean(id) && basename(id) === id && !id.includes("..") && !id.includes("/") && !id.includes("\\");
}

function subagentResultFromRecord(run: SubagentRunRecord, maxChars?: number): NonNullable<SubagentResultRead["run"]> {
	const capped = readCappedFile(run.resultPath, normalizeResultMaxChars(maxChars));
	return {
		id: run.id,
		label: run.label,
		mode: run.mode,
		status: run.status,
		cwd: run.cwd,
		...(capped.text ? { result: capped.text } : {}),
		...(capped.truncated ? { resultTruncated: true } : {}),
		transcriptTail: readTail(run.transcriptPath, RESULT_TAIL_LINES, RESULT_TAIL_CHARS),
		stderrTail: readTail(run.stderrPath, RESULT_TAIL_LINES, RESULT_TAIL_CHARS),
		error: run.error,
		exitCode: run.exitCode,
		artifactDir: run.artifactDir,
		promptPath: run.promptPath,
		resultPath: run.resultPath,
		transcriptPath: run.transcriptPath,
		stderrPath: run.stderrPath,
		metadataPath: run.metadataPath,
		...(run.terminal?.attachCommand ? { attachCommand: run.terminal.attachCommand } : {}),
	};
}

function normalizeResultTimeoutMs(value: number | undefined): number {
	if (!Number.isFinite(value)) return DEFAULT_RESULT_TIMEOUT_MS;
	return Math.max(0, Math.min(600_000, Math.floor(value as number)));
}

function normalizeResultMaxChars(value: number | undefined): number {
	if (!Number.isFinite(value)) return DEFAULT_RESULT_MAX_CHARS;
	return Math.max(1, Math.min(MAX_RESULT_MAX_CHARS, Math.floor(value as number)));
}

function readCappedFile(path: string, maxChars: number): { text: string; truncated: boolean } {
	try {
		const value = readFileSync(path, "utf-8").trim();
		return value.length > maxChars ? { text: value.slice(0, maxChars), truncated: true } : { text: value, truncated: false };
	} catch {
		return { text: "", truncated: false };
	}
}

function readTail(path: string, maxLines: number, maxChars: number): string | undefined {
	try {
		const lines = readFileSync(path, "utf-8").trimEnd().split("\n").slice(-maxLines).join("\n");
		if (!lines) return undefined;
		return lines.length > maxChars ? lines.slice(-maxChars) : lines;
	} catch {
		return undefined;
	}
}

function terminalCleanupStatus(status: SubagentStatus, processResult?: SubagentProcessResult): SubagentTerminalMetadata["cleanupStatus"] {
	if (processResult?.terminalCleanupStatus) return processResult.terminalCleanupStatus;
	if (status === "success" || status === "killed" || status === "timeout") return "cleaned";
	return "kept";
}

export function buildPiSubprocessInvocation(promptPath: string): SubagentInvocation {
	return buildPiSubprocessInvocationFromPromptFile(promptPath);
}

const runInvocation: SubagentCommandRunner = async (invocation, cwd, signal) => {
	let stdout = "";
	let stderr = "";
	let wasAborted = false;
	const exitCode = await new Promise<number>((resolve, reject) => {
		const proc = spawn(invocation.command, invocation.args, { cwd, shell: false, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, ...(invocation.env ?? {}) } });
		proc.stdout.on("data", (data) => { stdout += data.toString(); });
		proc.stderr.on("data", (data) => { stderr += data.toString(); });
		let closed = false;
		proc.on("error", reject);
		proc.on("close", (code) => {
			closed = true;
			resolve(code ?? (wasAborted ? 1 : 0));
		});
		const abort = () => {
			wasAborted = true;
			proc.kill("SIGTERM");
			setTimeout(() => {
				if (!closed) proc.kill("SIGKILL");
			}, 5000);
		};
		if (signal?.aborted) abort();
		else signal?.addEventListener("abort", abort, { once: true });
	});
	return { exitCode, stdout, stderr };
};

export async function defaultSubagentProcessRunner(
	request: SubagentProcessRequest,
	signal?: AbortSignal,
	lifecycle?: SubagentProcessLifecycle,
): Promise<SubagentProcessResult> {
	const invocation = buildPiSubprocessInvocation(request.promptPath);
	let stdout = "";
	let stderr = "";
	let wasAborted = false;

	const exitCode = await new Promise<number>((resolve, reject) => {
		const proc = spawn(invocation.command, invocation.args, {
			cwd: request.cwd,
			shell: false,
			stdio: ["ignore", "pipe", "pipe"],
			env: { ...process.env, ...(invocation.env ?? {}) },
		});
		attachPostExitStdioGuard(proc);

		proc.stdout.on("data", (data) => {
			const text = data.toString();
			stdout += text;
			writeFileSync(request.transcriptPath, text, { encoding: "utf-8", flag: "a" });
		});
		proc.stderr.on("data", (data) => {
			const text = data.toString();
			stderr += text;
			writeFileSync(request.stderrPath, text, { encoding: "utf-8", flag: "a" });
		});
		let closed = false;
		proc.on("spawn", () => lifecycle?.onStarted?.({ pid: proc.pid, invocation }));
		proc.on("error", reject);
		proc.on("close", (code) => {
			closed = true;
			resolve(code ?? (wasAborted ? 1 : 0));
		});

		const abort = () => {
			wasAborted = true;
			proc.kill("SIGTERM");
			setTimeout(() => {
				if (!closed) proc.kill("SIGKILL");
			}, 5000);
		};
		if (signal?.aborted) abort();
		else signal?.addEventListener("abort", abort, { once: true });
	});

	return {
		exitCode,
		stdout,
		stderr,
		result: extractFinalTextFromJsonLines(stdout) ?? stdout.trim(),
		transcript: stdout,
		invocation,
	};
}

export async function defaultTmuxSubagentProcessRunner(
	request: SubagentProcessRequest,
	signal?: AbortSignal,
	lifecycle?: SubagentProcessLifecycle,
): Promise<SubagentProcessResult> {
	if (!request.tmuxSpec) throw new Error("Terminal workspace spec is required");
	const start = await runInvocation(request.tmuxSpec.start, request.cwd, signal);
	if (start.exitCode !== 0) {
		return { exitCode: start.exitCode, stdout: start.stdout, stderr: start.stderr, invocation: request.tmuxSpec.start };
	}
	lifecycle?.onStarted?.({ invocation: request.tmuxSpec.start });

	let aborted = false;
	let cleanupPromise: Promise<{ exitCode: number; stdout: string; stderr: string }> | undefined;
	const abort = () => {
		aborted = true;
		cleanupPromise = runInvocation(buildTmuxKillInvocation(request.tmuxSpec!.terminal.sessionName), request.cwd).catch((error) => ({ exitCode: 1, stdout: "", stderr: error instanceof Error ? error.message : String(error) }));
	};
	if (signal?.aborted) abort();
	else signal?.addEventListener("abort", abort, { once: true });

	while (!aborted && !existsSync(request.tmuxSpec.exitCodePath)) {
		await new Promise((resolve) => setTimeout(resolve, 500));
	}
	const cleanup = cleanupPromise ? await cleanupPromise : undefined;
	const exitCode = aborted ? 1 : Number(readFileSync(request.tmuxSpec.exitCodePath, "utf-8").trim());
	const stdout = existsSync(request.transcriptPath) ? readFileSync(request.transcriptPath, "utf-8") : "";
	const stderr = existsSync(request.stderrPath) ? readFileSync(request.stderrPath, "utf-8") : "";
	return {
		exitCode: Number.isFinite(exitCode) ? exitCode : 1,
		stdout,
		stderr: cleanup && cleanup.exitCode !== 0 ? `${stderr}${stderr ? "\n" : ""}${cleanup.stderr}` : stderr,
		result: extractFinalTextFromJsonLines(stdout) ?? stdout.trim(),
		transcript: stdout,
		invocation: request.tmuxSpec.start,
		terminalCleanupStatus: cleanup ? (cleanup.exitCode === 0 ? "cleaned" : "failed") : undefined,
	};
}

type WidgetValue = string[] | ((...args: unknown[]) => { render: (width: number) => string[]; invalidate: () => void }) | undefined;

function notifySubagentUiState(ctx: { ui?: { setStatus?: (key: string, value: string | undefined) => void; setWidget?: (key: string, value: WidgetValue, options?: { placement?: "aboveEditor" | "belowEditor" }) => void } }, agentDir: string): void {
	try {
		const runs = listSubagentRuns(agentDir);
		const running = runs.filter((run) => run.status === "running").length;
		ctx.ui?.setStatus?.("subagents", running > 0 ? `subagents: ${running} running` : undefined);
		ctx.ui?.setWidget?.(
			"subagents",
			running > 0
				? () => ({ render: (width: number) => renderRunningSubagentsWidget(runs, { width }) ?? [], invalidate: () => {} })
				: undefined,
			{ placement: "aboveEditor" },
		);
	} catch {
		// Best-effort UI state; registry remains source of truth.
	}
}

export function startSubagentsOverlayRefresh(
	callbacks: { requestRender: () => void; updateUi?: () => void },
	timers: IntervalTimerApi = { setInterval, clearInterval },
): () => void {
	const timer = timers.setInterval(() => {
		callbacks.updateUi?.();
		callbacks.requestRender();
	}, 1_000);
	return () => timers.clearInterval(timer);
}

function safeNotify(ctx: { ui?: { notify?: (message: string, level?: "info" | "success" | "error" | "warning") => void } }, message: string, level: "info" | "success" | "error" | "warning"): void {
	try {
		ctx.ui?.notify?.(message, level);
	} catch {
		// Completion notifications are best-effort; artifacts and registry are durable.
	}
}

function createSubagentsOverlay(agentDir: string, done: () => void, requestRender?: () => void) {
	let state: SubagentUiState = { selectedIndex: 0, detail: false };
	return {
		render(width: number): string[] {
			const runs = listSubagentRuns(agentDir);
			return state.detail
				? renderSubagentDetailLines(runs[state.selectedIndex], { width })
				: renderSubagentListLines(runs, { selectedIndex: state.selectedIndex, width, confirm: state.confirm });
		},
		handleInput(data: string): void {
			const runs = listSubagentRuns(agentDir);
			const next = reduceSubagentUiState(state, data, runs);
			state = next.state;
			if (next.action?.type === "close") {
				done();
				return;
			}
			if (next.action?.type === "dismiss") dismissSubagentRunRecord(agentDir, next.action.id);
			if (next.action?.type === "dismiss-completed") dismissCompletedSubagentRunRecords(agentDir);
			if (next.action?.type === "kill") void killWorkspace(agentDir, next.action.id).finally(() => requestRender?.());
			if (next.action?.type === "cleanup") void cleanupWorkspaceTerminal(agentDir, next.action.id).finally(() => requestRender?.());
			requestRender?.();
		},
		invalidate(): void {},
	};
}

export default function subagentRunnerExtension(
	pi: ExtensionAPI,
	runner: SubagentProcessRunner = defaultSubagentProcessRunner,
) {
	if (process.env[SUBAGENT_CHILD_ENV] === "1") return;
	pi.on?.("session_start", (_event, ctx) => {
		const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
		notifySubagentUiState(ctx, agentDir);
	});
	pi.registerCommand("subagent", {
		description: "Run one focused subagent task in an isolated headless pi process",
		handler: async (args, ctx) => {
			const parsed = parseSubagentCommandArgs(args);
			if (!parsed.ok) {
				safeNotify(ctx, parsed.error, "info");
				return;
			}
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			try {
				const run = runSubagentTask({
					prompt: parsed.prompt,
					label: parsed.label ?? "manual",
					cwd: ctx.cwd,
					timeoutSeconds: parsed.timeoutSeconds,
					terminal: parsed.terminal,
					agentDir,
					onStart: (record) => {
						safeNotify(ctx, formatSubagentStartNotification(record), "info");
						notifySubagentUiState(ctx, agentDir);
					},
				}, runner);
				void run.then((result) => {
					notifySubagentUiState(ctx, agentDir);
					safeNotify(ctx, formatSubagentCompletionNotification(result), subagentNotificationLevel(result.status));
				}).catch((error) => {
					notifySubagentUiState(ctx, agentDir);
					safeNotify(ctx, error instanceof Error ? error.message : String(error), "error");
				});
			} catch (error) {
				notifySubagentUiState(ctx, agentDir);
				safeNotify(ctx, error instanceof Error ? error.message : String(error), "error");
			}
		},
	});

	pi.registerCommand("subagents", {
		description: "Show running and recent subagent runs",
		handler: async (_args, ctx) => {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			notifySubagentUiState(ctx, agentDir);
			if (typeof ctx.ui.custom === "function") {
				let requestRender = () => {};
				const stopRefresh = startSubagentsOverlayRefresh({
					requestRender: () => requestRender(),
					updateUi: () => notifySubagentUiState(ctx, agentDir),
				});
				try {
					await ctx.ui.custom((tui, _theme, _keybindings, done) => {
						requestRender = () => tui.requestRender();
						return createSubagentsOverlay(agentDir, done, requestRender);
					}, { overlay: true });
				} finally {
					stopRefresh();
				}
				return;
			}
			safeNotify(ctx, formatSubagentRuns(listSubagentRuns(agentDir)), "info");
		},
	});

	pi.registerTool({
		name: "subagent_start",
		label: "Subagent Start",
		description: "Start a focused subagent in the background and return immediately. Prefer this for parallel work.",
		promptSnippet: "Start a background subagent task",
		parameters: SubagentStartParams,
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			try {
				const workspace = startSubagentWorkspace({
					prompt: params.prompt,
					label: params.label,
					cwd: params.cwd ?? ctx.cwd,
					timeoutSeconds: params.timeoutSeconds,
					terminal: params.terminal,
					allowMutations: params.allowMutations,
					promptPolicy: "background",
					agentDir,
					onStart: () => notifySubagentUiState(ctx, agentDir),
				}, runner);
				const started = await workspace.started;
				void workspace.completion.then((result) => {
					notifySubagentUiState(ctx, agentDir);
					safeNotify(ctx, formatSubagentCompletionNotification(result), subagentNotificationLevel(result.status));
				}).catch((error) => {
					notifySubagentUiState(ctx, agentDir);
					safeNotify(ctx, error instanceof Error ? error.message : String(error), "error");
				});
				const guidance = formatSubagentStartReport(started);
				return textResult(guidance, started, started.status === "failed_to_start");
			} catch (error) {
				notifySubagentUiState(ctx, agentDir);
				return textResult(error instanceof Error ? error.message : String(error), { error: String(error) }, true);
			}
		},
	});

	pi.registerTool({
		name: "subagent_result",
		label: "Subagent Result",
		description: "Poll or wait for a background subagent result. Defaults to non-blocking polling.",
		promptSnippet: "Read a subagent result",
		parameters: SubagentResultParams,
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			const result = await readSubagentResult(agentDir, params.id, { block: params.block, timeoutMs: params.timeoutMs, maxChars: params.maxChars, signal });
			return textResult(formatSubagentResultRead(result), result, result.retrieval_status === "not_found");
		},
	});

	pi.registerTool({
		name: "subagent_list",
		label: "Subagent List",
		description: "List running and recent subagent workspaces for orchestration.",
		promptSnippet: "List subagent workspaces",
		parameters: SubagentListParams,
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			const runs = filterSubagentRuns(listSubagentRuns(agentDir), params.status ?? "all", params.limit ?? 20);
			return textResult(formatSubagentRuns(runs), { runs }, false);
		},
	});

	pi.registerTool({
		name: "subagent_kill",
		label: "Subagent Kill",
		description: "Stop a running subagent workspace by id. Requires a reason.",
		promptSnippet: "Kill a running subagent",
		parameters: SubagentKillParams,
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			if (!params.reason?.trim()) return textResult("subagent_kill requires a non-empty reason", { error: "reason required" }, true);
			const killed = await killWorkspace(agentDir, params.id, params.reason);
			notifySubagentUiState(ctx, agentDir);
			const result = await readSubagentResult(agentDir, params.id, { block: false });
			return textResult(killed.ok ? `Subagent ${params.id} killed: ${params.reason}` : killed.error, { ...killed, result }, !killed.ok);
		},
	});

	pi.registerTool({
		name: "subagent_run",
		label: "Subagent Run And Wait",
		description: "Run one focused subagent task and wait for completion. Use subagent_start instead for parallel/background work.",
		promptSnippet: "Run a focused subagent task and wait",
		parameters: SubagentRunParams,
		async execute(_toolCallId, params, _signal, onUpdate, ctx) {
			const agentDir = (ctx as unknown as { agentDir?: string }).agentDir ?? getAgentDir();
			try {
				const result = await runSubagentTask(
					{
						prompt: params.prompt,
						label: params.label,
						cwd: params.cwd ?? ctx.cwd,
						timeoutSeconds: params.timeoutSeconds,
						terminal: params.terminal,
						agentDir,
						onStart: (record) => {
							onUpdate?.({ content: [{ type: "text" as const, text: `Subagent ${record.label} started. Artifacts: ${record.artifactDir}` }] });
							notifySubagentUiState(ctx, agentDir);
						},
					},
					runner,
				);
				notifySubagentUiState(ctx, agentDir);
				return textResult(formatSubagentReport(result), result, subagentToolResultIsError(result.status));
			} catch (error) {
				notifySubagentUiState(ctx, agentDir);
				return textResult(error instanceof Error ? error.message : String(error), { error: String(error) }, true);
			}
		},
	});
}

function formatSubagentStartReport(result: SubagentStartResult): string {
	const lines = [`Subagent ${result.label} ${result.status === "started" ? "started" : "failed to start"}.`, "", `id: ${result.id}`, `status: ${result.status}`, "", "Artifacts:", `- result: ${result.resultPath}`, `- transcript: ${result.transcriptPath}`, `- metadata: ${result.metadataPath}`];
	if (result.attachCommand) lines.push(`- attach: ${result.attachCommand}`);
	if (result.error) lines.push("", `Error: ${result.error}`);
	if (result.status === "started") lines.push("", "The subagent is running in the background. Do not duplicate its exact work; continue non-overlapping work or launch other independent workers. Use subagent_result with block:false to poll, or block:true only when ready to join.");
	return lines.join("\n");
}

function formatSubagentResultRead(result: SubagentResultRead): string {
	if (!result.run) return `Subagent result: ${result.retrieval_status}`;
	const lines = [`Subagent ${result.run.id}: ${result.retrieval_status}`, `status: ${result.run.status}`];
	if (result.run.result) lines.push("", "Result:", result.run.result);
	if (result.run.resultTruncated) lines.push("", `(Result truncated. Full result: ${result.run.resultPath})`);
	if (result.run.error) lines.push("", `Error: ${result.run.error}`);
	lines.push("", "Artifacts:", `- result: ${result.run.resultPath}`, `- transcript: ${result.run.transcriptPath}`, `- metadata: ${result.run.metadataPath}`);
	return lines.join("\n");
}

function extractFinalTextFromJsonLines(output: string): string | undefined {
	let finalText: string | undefined;
	for (const line of output.split("\n")) {
		if (!line.trim()) continue;
		try {
			const event = JSON.parse(line) as { message?: { role?: string; content?: Array<{ type?: string; text?: string }> } };
			if (event.message?.role !== "assistant") continue;
			for (const part of event.message.content ?? []) {
				if (part.type === "text" && part.text) finalText = part.text;
			}
		} catch {
			// Not JSON-mode output; ignore and fall back to raw stdout.
		}
	}
	return finalText?.trim();
}

function extractPartialTextFromJsonLines(output: string, maxChars = 4_000): string | undefined {
	let partialText: string | undefined;
	for (const line of output.split("\n")) {
		if (!line.trim()) continue;
		try {
			const event = JSON.parse(line) as {
				message?: { role?: string; content?: Array<{ type?: string; text?: string }> };
				partialResult?: { content?: Array<{ type?: string; text?: string }> };
				result?: { content?: Array<{ type?: string; text?: string }> };
			};
			const content = event.partialResult?.content ?? event.result?.content ?? (event.message?.role === "assistant" || event.message?.role === "toolResult" ? event.message.content : undefined);
			for (const part of content ?? []) {
				if (part.type === "text" && part.text?.trim()) partialText = part.text.trim();
			}
		} catch {
			// Not JSON-mode output; ignore and fall back below.
		}
	}
	const fallback = partialText ?? output.trim();
	if (!fallback) return undefined;
	return fallback.length > maxChars ? fallback.slice(-maxChars) : fallback;
}

function formatTimeoutPartialResult(error: string, transcript: string): string {
	const lines = [error.replace(/^Subagent\s+/i, "Subagent "), ""];
	const partial = extractPartialTextFromJsonLines(transcript);
	if (partial) lines.push("Partial transcript tail:", partial);
	else lines.push("No final response was produced before timeout. Inspect transcript for details.");
	return lines.join("\n").trim();
}

function textResult(text: string, details: Record<string, unknown>, isError = false) {
	return { content: [{ type: "text" as const, text }], details, isError };
}
