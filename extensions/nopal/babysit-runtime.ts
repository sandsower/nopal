import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { ledgerEvent, ledgerFinalize, ledgerInit, type ExecFn, type LedgerFinalizeStatus } from "./nopal-cli.js";
import { parseTokenBudgetArg, splitBabysitTokenBudgetArg, type NopalConfigCache } from "./nopal-config.js";
import type { WorkflowSignals } from "./workflow-signals.js";

const RUN_ENTRY = "nopal-babysit-run";
const EVENT_ENTRY = "nopal-babysit-event";
const TOOL_NAMES = ["get_nopal_babysit", "update_nopal_babysit"];
const LEDGER_FLOW = "babysit";

type BabysitStatus = "active" | "complete" | "blocked" | "budget_limited";

type BabysitRun = {
	version: 1;
	id: string;
	args: string;
	status: BabysitStatus;
	tokenBudget: number | null;
	tokensUsed: number;
	createdAt: number;
	updatedAt: number;
	summary?: string;
	/** Durable run ledger id (`nopal ledger init`), when ledger recording succeeded. */
	ledgerRunId?: string;
};

type UsageSnapshot = {
	totalTokens?: number;
	input?: number;
	output?: number;
	cacheRead?: number;
	cacheWrite?: number;
} | null | undefined;

let run: BabysitRun | null = null;
let continuationQueued = false;

function notify(ctx: ExtensionContext, message: string, level: "info" | "warning" | "error" | "success" = "info") {
	if (ctx.hasUI) ctx.ui.notify(message, level);
}

function tokenDeltaFromUsage(usage: UsageSnapshot): number {
	if (!usage) return 0;
	if (typeof usage.totalTokens === "number") return Math.max(0, usage.totalTokens);
	const input = Number(usage.input) || 0;
	const output = Number(usage.output) || 0;
	const cacheRead = Number(usage.cacheRead) || 0;
	const cacheWrite = Number(usage.cacheWrite) || 0;
	return Math.max(0, input + output + cacheRead + cacheWrite);
}

function formatTokens(value: number): string {
	if (value >= 1_000_000) return `${Math.round(value / 100_000) / 10}M`;
	if (value >= 1_000) return `${Math.round(value / 100) / 10}K`;
	return String(value);
}

function syncTools(pi: ExtensionAPI) {
	const active = new Set(pi.getActiveTools());
	const want = run?.status === "active";
	for (const name of TOOL_NAMES) (want ? active.add(name) : active.delete(name));
	pi.setActiveTools(Array.from(active));
}

function persist(pi: ExtensionAPI, ctx: ExtensionContext, next: BabysitRun | null) {
	run = next;
	pi.appendEntry(RUN_ENTRY, { run: next });
	syncTools(pi);
	if (ctx.hasUI) {
		if (!run) ctx.ui.setStatus(RUN_ENTRY, "");
		else {
			const budget = run.tokenBudget == null ? "" : ` (${formatTokens(run.tokensUsed)} / ${formatTokens(run.tokenBudget)})`;
			ctx.ui.setStatus(RUN_ENTRY, run.status === "active" ? `Babysitting PR${budget}` : `Babysit ${run.status}${budget}`);
		}
	}
}

function latestRunFromSession(ctx: ExtensionContext): BabysitRun | null {
	const entries = ctx.sessionManager.getBranch?.() ?? ctx.sessionManager.getEntries();
	for (let i = entries.length - 1; i >= 0; i--) {
		const entry = entries[i] as { type?: string; customType?: string; data?: { run?: BabysitRun | null } };
		if (entry.type === "custom" && entry.customType === RUN_ENTRY) return entry.data?.run ?? null;
	}
	return null;
}

function emitEvent(pi: ExtensionAPI, kind: BabysitStatus, current: BabysitRun) {
	pi.sendMessage({
		customType: EVENT_ENTRY,
		content: `Nopal babysit status: ${kind}\n\nArgs: ${current.args || "(none)"}${current.summary ? `\n\nSummary: ${current.summary}` : ""}`,
		display: true,
		details: { kind, run: current, timestamp: Date.now() },
	});
}

function startPrompt(current: BabysitRun): string {
	return `Load and follow the Nopal babysit skill for the current pull request.

Invocation args: ${current.args || "(none)"}

Pi babysit persistence is active for this run. Use get_nopal_babysit only if you need the current run state. When the babysit workflow reaches its configured green endpoint, after final audit and configured closeout are complete, call update_nopal_babysit({status:"complete", summary:"..."}). If you hit a human-decision, policy, credential, conflict, or unsafe-blocker stop condition, call update_nopal_babysit({status:"blocked", summary:"..."}) and explain the blocker. Do not call update_nopal_babysit while substantive babysit work remains.`;
}

function continuationPrompt(current: BabysitRun): string {
	const tokenBudget = current.tokenBudget == null ? "none" : String(current.tokenBudget);
	const remainingTokens = current.tokenBudget == null ? "n/a" : String(Math.max(0, current.tokenBudget - current.tokensUsed));
	return `Continue the active Nopal babysit workflow for the current pull request.

Invocation args: ${current.args || "(none)"}

Budget:
- Tokens used: ${current.tokensUsed}
- Token budget: ${tokenBudget}
- Tokens remaining: ${remainingTokens}

Re-read live PR evidence before deciding what to do next. Continue using the babysit skill workflow. Only call update_nopal_babysit({status:"complete"}) after the configured green endpoint and final audit are actually complete. If blocked on human judgment, policy approval, credentials, conflicts, or unsafe state, call update_nopal_babysit({status:"blocked"}) with a summary.`;
}

function queueContinuation(pi: ExtensionAPI, current: BabysitRun) {
	if (continuationQueued || current.status !== "active") return;
	continuationQueued = true;
	queueMicrotask(() => {
		continuationQueued = false;
		if (!run || run.id !== current.id || run.status !== "active") return;
		pi.sendUserMessage(continuationPrompt(run), { deliverAs: "followUp" });
	});
}

/**
 * Best-effort `nopal ledger init` for a new babysit run. Failures notify
 * the UI and return undefined; the babysit loop proceeds without a durable
 * ledger record rather than blocking on it.
 */
async function startLedgerRun(exec: ExecFn, ctx: ExtensionContext): Promise<string | undefined> {
	const result = await ledgerInit(exec, { skill: "babysit", flow: LEDGER_FLOW, cwd: ctx.cwd });
	if (!result.ok) {
		notify(ctx, `Nopal ledger: could not start a babysit run record (${result.error}). Continuing without ledger recording.`, "warning");
		return undefined;
	}
	return result.runId;
}

/** Best-effort `nopal ledger event`; notifies on failure but never throws. */
async function recordLedgerEvent(exec: ExecFn, ctx: ExtensionContext, runId: string, type: string, summary: string): Promise<void> {
	const result = await ledgerEvent(exec, { runId, flow: LEDGER_FLOW, type, summary, cwd: ctx.cwd });
	if (!result.ok) notify(ctx, `Nopal ledger: failed to record ${type} (${result.error}).`, "warning");
}

/** Best-effort `nopal ledger finalize`; notifies on failure but never throws. */
async function finalizeLedgerRun(exec: ExecFn, ctx: ExtensionContext, runId: string, status: LedgerFinalizeStatus): Promise<void> {
	const result = await ledgerFinalize(exec, { runId, flow: LEDGER_FLOW, status, cwd: ctx.cwd });
	if (!result.ok) notify(ctx, `Nopal ledger: failed to finalize the babysit run (${result.error}).`, "warning");
}

function finalizeStatusFor(status: BabysitStatus): LedgerFinalizeStatus | undefined {
	if (status === "complete") return "completed";
	if (status === "blocked" || status === "budget_limited") return "interrupted";
	return undefined;
}

export function registerBabysitRuntime(pi: ExtensionAPI, configCache: NopalConfigCache, workflowSignals: WorkflowSignals) {
	const exec: ExecFn = (command, args, options) => pi.exec(command, args, options);

	async function closeOutLedgerIfTerminal(ctx: ExtensionContext, current: BabysitRun, reason?: string): Promise<void> {
		const finalizeStatus = finalizeStatusFor(current.status);
		if (!finalizeStatus || !current.ledgerRunId) {
			if (finalizeStatus) workflowSignals.setActiveLedgerRun(undefined);
			return;
		}
		if (reason) await recordLedgerEvent(exec, ctx, current.ledgerRunId, `babysit_${current.status}`, reason);
		await finalizeLedgerRun(exec, ctx, current.ledgerRunId, finalizeStatus);
		workflowSignals.setActiveLedgerRun(undefined);
	}

	pi.registerTool({
		name: "get_nopal_babysit",
		label: "Get Nopal Babysit Run",
		description: "Read the active Nopal babysit run state.",
		promptSnippet: "Read current Nopal babysit loop status and budget",
		promptGuidelines: ["Only call this when you need the current babysit loop status; the continuation prompt usually includes enough context."],
		parameters: Type.Object({}),
		async execute() {
			return { content: [{ type: "text", text: JSON.stringify({ run }, null, 2) }], details: { run } };
		},
	});

	pi.registerTool({
		name: "update_nopal_babysit",
		label: "Update Nopal Babysit Run",
		description: "Mark the active Nopal babysit run complete or blocked.",
		promptSnippet: "Complete or block the active Nopal babysit loop after audit",
		promptGuidelines: [
			"Call with status=complete only when the configured babysit green endpoint and final audit are complete.",
			"Call with status=blocked when babysit cannot proceed without human judgment, credentials, policy approval, or unsafe conflict handling.",
		],
		parameters: Type.Object({
			status: Type.Union([Type.Literal("complete"), Type.Literal("blocked")]),
			summary: Type.Optional(Type.String()),
		}),
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			if (!run || run.status !== "active") {
				return { content: [{ type: "text", text: "No active Nopal babysit run." }], isError: true };
			}
			const next: BabysitRun = { ...run, status: params.status, summary: params.summary, updatedAt: Date.now() };
			persist(pi, ctx, next);
			emitEvent(pi, params.status, next);
			await closeOutLedgerIfTerminal(ctx, next, params.summary);
			return { content: [{ type: "text", text: JSON.stringify({ run: next }, null, 2) }], details: { run: next } };
		},
	});

	pi.on("session_start", (_event, ctx) => {
		run = latestRunFromSession(ctx);
		continuationQueued = false;
		syncTools(pi);
		if (run?.status === "active" && run.ledgerRunId) {
			workflowSignals.setActiveLedgerRun({ runId: run.ledgerRunId, flow: LEDGER_FLOW, cwd: ctx.cwd });
		} else {
			workflowSignals.setActiveLedgerRun(undefined);
		}
	});

	pi.on("turn_end", (event, ctx) => {
		if (!run || run.status !== "active") return;
		let next: BabysitRun = { ...run, tokensUsed: run.tokensUsed + tokenDeltaFromUsage((event.message as { usage?: UsageSnapshot } | undefined)?.usage), updatedAt: Date.now() };
		if (next.tokenBudget != null && next.tokensUsed >= next.tokenBudget) next = { ...next, status: "budget_limited" };
		persist(pi, ctx, next);
		if (next.ledgerRunId) void recordLedgerEvent(exec, ctx, next.ledgerRunId, "babysit_turn", `${next.tokensUsed} tokens used`);
		if (next.status === "budget_limited") {
			emitEvent(pi, "budget_limited", next);
			void closeOutLedgerIfTerminal(ctx, next, `budget limited at ${next.tokensUsed} tokens (budget ${next.tokenBudget})`);
		}
	});

	pi.on("agent_end", (_event, ctx) => {
		if (!run || run.status !== "active" || ctx.hasPendingMessages()) return;
		queueContinuation(pi, run);
	});

	return {
		async start(args: string, ctx: ExtensionContext) {
			const parsed = splitBabysitTokenBudgetArg(args);
			const config = await configCache.get(ctx.cwd);
			const tokenBudget = parsed.tokenBudget !== undefined ? parseTokenBudgetArg(parsed.tokenBudget) : config.babysitTokenBudget;
			const now = Date.now();
			let next: BabysitRun = {
				version: 1,
				id: `${now}-${Math.random().toString(16).slice(2)}`,
				args: parsed.args,
				status: "active",
				tokenBudget,
				tokensUsed: 0,
				createdAt: now,
				updatedAt: now,
			};
			const ledgerRunId = await startLedgerRun(exec, ctx);
			if (ledgerRunId) {
				next = { ...next, ledgerRunId };
				workflowSignals.setActiveLedgerRun({ runId: ledgerRunId, flow: LEDGER_FLOW, cwd: ctx.cwd });
			}
			persist(pi, ctx, next);
			emitEvent(pi, "active", next);
			await pi.sendUserMessage(startPrompt(next), { deliverAs: "followUp" });
		},
	};
}
