/**
 * Workflow signal surfacing for the Nopal session extension.
 *
 * Ported from beislid's `workflow-signals.ts`: UI status/title surfacing
 * (`surfaceWorkflowSignal`), the initial per-skill signal map
 * (`INITIAL_SKILL_SIGNALS`), and bash-command mirroring
 * (`surfaceWorkflowSignalsFromCommand`) are unchanged in spirit.
 *
 * Two additions on top of the beislid behavior:
 * - When a ledger run is active (babysit today, future skill runs later),
 *   every surfaced signal also appends a best-effort `nopal ledger event
 *   --type workflow_signal` so the durable record captures state
 *   transitions, not just the UI.
 * - The legacy `beislid workflow-signal emit` shell-out (kept for
 *   tmux-glance continuity until a ledger-tail sink exists) is gated
 *   behind the `NOPAL_SIGNALS_BEISLID_CLI` env flag, default on.
 */

import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { ledgerEvent, type ExecFn } from "./nopal-cli.js";
import type { NopalSkill } from "./skill-commands.js";

export const WORKFLOW_SIGNAL_STATES = ["working", "blocked", "waiting", "verify", "review", "done", "explore"] as const;
export type WorkflowSignalState = (typeof WORKFLOW_SIGNAL_STATES)[number];

export type WorkflowSignal = {
	state: WorkflowSignalState;
	skill?: NopalSkill;
	phase?: string;
	event?: string;
};

const STATE_LABELS: Record<WorkflowSignalState, string> = {
	working: "🛠 working",
	blocked: "⛔ blocked",
	waiting: "⏳ waiting",
	verify: "🧪 verify",
	review: "👀 review",
	done: "✅ done",
	explore: "🔎 explore",
};

export const INITIAL_SKILL_SIGNALS: Partial<Record<NopalSkill, WorkflowSignalState>> = {
	babysit: "working",
	blueprint: "working",
	"break-spec": "working",
	debug: "explore",
	doctor: "verify",
	"fresh-eyes": "review",
	handoff: "working",
	implement: "working",
	kickoff: "working",
	"poke-holes": "working",
	"pr-patrol": "review",
	"ready-for-review": "working",
	retro: "review",
	review: "review",
	"review-response": "working",
	rinse: "review",
	setup: "working",
	"show-me": "working",
	spec: "working",
	verify: "verify",
	"walk-the-diff": "review",
};

export function initialSignalForSkill(skill: NopalSkill): WorkflowSignal | undefined {
	const state = INITIAL_SKILL_SIGNALS[skill];
	return state ? { state, skill, phase: "start" } : undefined;
}

function titleForSignal(signal: WorkflowSignal): string {
	const skill = signal.skill ? ` ${signal.skill}` : "";
	const phase = signal.phase ? `:${signal.phase}` : "";
	return `Nopal ${STATE_LABELS[signal.state]}${skill}${phase}`;
}

/** Pure UI surfacing: status bar text and window title. No side effects beyond `ctx.ui`. */
export function surfaceWorkflowSignal(ctx: ExtensionContext, signal: WorkflowSignal) {
	if (!ctx.hasUI) return;
	const title = titleForSignal(signal);
	ctx.ui.setStatus("nopal-workflow", title);
	ctx.ui.setTitle(title);
}

const BEISLID_CLI_ENV_VAR = "NOPAL_SIGNALS_BEISLID_CLI";

/** Default on; any value other than "0"/"false" (case-insensitive) keeps it on. */
export function beislidCliShellOutEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
	const raw = env[BEISLID_CLI_ENV_VAR];
	if (raw === undefined) return true;
	const normalized = raw.trim().toLowerCase();
	return normalized !== "0" && normalized !== "false";
}

function shellOutToBeislidCli(ctx: ExtensionContext, signal: WorkflowSignal, env: NodeJS.ProcessEnv) {
	if (!beislidCliShellOutEnabled(env)) return;
	const args = ["workflow-signal", "emit", signal.state];
	if (signal.skill) args.push("--skill", signal.skill);
	if (signal.phase) args.push("--phase", signal.phase);
	if (signal.event) args.push("--event", signal.event);
	args.push("--repo", ctx.cwd);

	execFile("beislid", args, { cwd: ctx.cwd, timeout: 2000 }, () => {
		// Best-effort local signal fan-out (tmux-glance continuity). Missing CLI,
		// unconfigured workflow_signals, non-tmux sessions, and sink failures
		// must not block the Pi workflow.
	});
}

function parseFlag(args: string, flag: "skill" | "phase" | "event"): string | undefined {
	const match = args.match(new RegExp(`(?:^|\\s)--${flag}(?:=|\\s+)(['"]?)([^'"\\s;&|]+)\\1`));
	return match?.[2];
}

/**
 * Pure parse of `beislid workflow-signal emit <state> [flags]` invocations
 * out of a shell command string. Skills still shell out to the legacy
 * `beislid` binary directly (their SKILL.md instructions are unchanged by
 * this port), so this mirrors those signals into Nopal's own UI/ledger.
 */
export function parseWorkflowSignalsFromCommand(command: string): WorkflowSignal[] {
	const signals: WorkflowSignal[] = [];
	const re = /(?:^|[\s;&|])beislid\s+workflow-signal\s+emit\s+(working|blocked|waiting|verify|review|done|explore)\b([^\n;&|]*)/g;
	let match: RegExpExecArray | null;
	while ((match = re.exec(command))) {
		const state = match[1] as WorkflowSignalState;
		const rest = match[2] ?? "";
		signals.push({
			state,
			skill: parseFlag(rest, "skill") as NopalSkill | undefined,
			phase: parseFlag(rest, "phase"),
			event: parseFlag(rest, "event"),
		});
	}
	return signals;
}

export type ActiveLedgerRun = {
	runId: string;
	flow?: string;
	cwd?: string;
};

export type WorkflowSignals = {
	/** Surface a signal the extension originated itself: UI + legacy CLI shell-out + ledger mirror. */
	emitWorkflowSignal(ctx: ExtensionContext, signal: WorkflowSignal): void;
	/** Mirror signals detected in a bash tool_call command: UI + ledger mirror (no re-shell-out). */
	surfaceWorkflowSignalsFromCommand(ctx: ExtensionContext, command: string): void;
	/** Register (or clear, with undefined) the ledger run signals should be mirrored into. */
	setActiveLedgerRun(run: ActiveLedgerRun | undefined): void;
};

/**
 * Build the workflow-signals surface bound to an exec seam (`pi.exec` in
 * production) and environment (`process.env` in production). Kept as a
 * factory rather than module-level state so tests can inject a fake exec
 * and a fake env without touching global process state.
 */
export function createWorkflowSignals(exec: ExecFn, env: NodeJS.ProcessEnv = process.env): WorkflowSignals {
	let activeRun: ActiveLedgerRun | undefined;

	function mirrorToLedger(signal: WorkflowSignal): void {
		if (!activeRun) return;
		const { runId, flow, cwd } = activeRun;
		void ledgerEvent(exec, { runId, flow, cwd, type: "workflow_signal", summary: signal.state }).catch(() => {
			// Best-effort; ledger mirroring of a UI signal never blocks the workflow.
		});
	}

	return {
		emitWorkflowSignal(ctx, signal) {
			surfaceWorkflowSignal(ctx, signal);
			shellOutToBeislidCli(ctx, signal, env);
			mirrorToLedger(signal);
		},
		surfaceWorkflowSignalsFromCommand(ctx, command) {
			for (const signal of parseWorkflowSignalsFromCommand(command)) {
				surfaceWorkflowSignal(ctx, signal);
				mirrorToLedger(signal);
			}
		},
		setActiveLedgerRun(run) {
			activeRun = run;
		},
	};
}
