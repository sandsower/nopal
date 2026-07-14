import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { registerAfkTools } from "./afk-tools.js";
import { registerBabysitRuntime } from "./babysit-runtime.js";
import { readLatestCheckpoint, pickNewBoundary, type BoundaryIdentity, type CheckpointPointerSnapshot } from "./checkpoints.js";
import { createNopalConfigCache } from "./nopal-config.js";
import { establishPlot, type ExecFn } from "./nopal-cli.js";
import { registerNopalSessionBridge } from "./session-bridge.js";
import {
	NOPAL_SKILLS,
	BOUNDARY_CAPABLE_SKILLS,
	commandNameForSkill,
	filterWrappedSkillAutocompleteItems,
	skillPrompt,
	type NopalSkill,
} from "./skill-commands.js";
import { createWorkflowSignals, initialSignalForSkill } from "./workflow-signals.js";

const CONSUMED_ENTRY = "nopal-auto-handoff-consumed";
const ESTABLISHMENT_PENDING_ENTRY = "nopal-plot-establishment-pending";
const ESTABLISHMENT_APPLIED_ENTRY = "nopal-plot-establishment-applied";
const ESTABLISHMENT_COMPLETE_ENTRY = "nopal-plot-establishment-complete";
const INTERNAL_HANDOFF_COMMAND = "nopal-auto-handoff";

type ManagedRun = {
	runId: string;
	skill: NopalSkill;
	command: string;
	startedAt: number;
	before?: CheckpointPointerSnapshot;
};

type EstablishmentAppliedEntry = {
	runId: string;
	boundary: BoundaryIdentity;
	plotId: string;
	establishedAt: string;
};

type EstablishmentCompleteEntry = {
	runId: string;
	completedAt: string;
};

type ConsumedEntry = {
	boundary: BoundaryIdentity;
	consumedAt: string;
};

function notify(ctx: ExtensionContext, message: string, level: "info" | "warning" | "error" | "success" = "info") {
	if (ctx.hasUI) ctx.ui.notify(message, level);
}

function refreshConsumed(ctx: ExtensionContext, consumed: Set<string>) {
	consumed.clear();
	for (const entry of ctx.sessionManager.getEntries() as Array<{ type?: string; customType?: string; data?: unknown }>) {
		if (entry.type !== "custom" || entry.customType !== CONSUMED_ENTRY) continue;
		const data = entry.data as Partial<ConsumedEntry> | undefined;
		const id = data?.boundary?.id;
		if (typeof id === "string") consumed.add(id);
	}
}

function customEntries(ctx: ExtensionContext): Array<{ customType?: string; data?: unknown }> {
	return (ctx.sessionManager.getEntries() as Array<{ type?: string; customType?: string; data?: unknown }>)
		.filter((entry) => entry.type === "custom");
}

function unresolvedEstablishmentRuns(ctx: ExtensionContext): ManagedRun[] {
	const entries = customEntries(ctx);
	const complete = new Set<string>();
	const applied = new Set<string>();
	for (const entry of entries) {
		const runId = (entry.data as { runId?: unknown } | undefined)?.runId;
		if (typeof runId !== "string") continue;
		if (entry.customType === ESTABLISHMENT_COMPLETE_ENTRY) complete.add(runId);
		if (entry.customType === ESTABLISHMENT_APPLIED_ENTRY) applied.add(runId);
	}
	const pending = new Map<string, ManagedRun>();
	for (const entry of entries) {
		if (entry.customType !== ESTABLISHMENT_PENDING_ENTRY) continue;
		const run = entry.data as Partial<ManagedRun> | undefined;
		if (typeof run?.runId !== "string" || typeof run.command !== "string" || typeof run.startedAt !== "number") continue;
		if (!NOPAL_SKILLS.includes(run.skill as NopalSkill)) continue;
		if (!complete.has(run.runId) && !applied.has(run.runId)) pending.set(run.runId, run as ManagedRun);
	}
	return [...pending.values()];
}

function continuationPrompt(boundary: BoundaryIdentity, workflow: string): string {
	return `Continue the Nopal ${workflow} workflow from a fresh Pi session.\n\nRead the checkpoint pointer (\`nopal ledger pointer\`), then read the referenced checkpoint artifact for event ${boundary.event} at ${boundary.path}. Use that artifact as the primary context seed. Do not synthesize missing context from prior chat history. Do not auto-handoff again for this same checkpoint boundary: ${boundary.id}.`;
}

export default function nopalExtension(pi: ExtensionAPI) {
	let activeRun: ManagedRun | undefined;
	let runSequence = 0;
	let pendingBoundary: BoundaryIdentity | undefined;
	let pendingWorkflow: string | undefined;
	const consumed = new Set<string>();
	const exec: ExecFn = (command, args, options) => pi.exec(command, args, options);
	const sessionBridge = registerNopalSessionBridge(pi, exec);
	registerAfkTools(pi, exec, Type as any);
	const configCache = createNopalConfigCache(exec);
	const workflowSignals = createWorkflowSignals(exec);
	const babysitRuntime = registerBabysitRuntime(pi, configCache, workflowSignals);
	const completeEstablishmentRun = (runId: string) => {
		pi.appendEntry<EstablishmentCompleteEntry>(ESTABLISHMENT_COMPLETE_ENTRY, {
			runId,
			completedAt: new Date().toISOString(),
		});
	};
	const applyEstablishment = async (run: ManagedRun, boundary: BoundaryIdentity, ctx: ExtensionContext): Promise<boolean> => {
		let result = await establishPlot(exec, {
			event: boundary.event,
			cwd: ctx.cwd,
		});
		if (!result.ok) {
			notify(ctx, result.error, "error");
			return false;
		}
		// Core is the sole authority for the Plot and selected Session identity.
		// Never persist an endpoint discovered before this establishment result.
		const selectedSessionId = result.envelope.plot.selected_session_id;
		if (typeof selectedSessionId === "string") {
			const endpoint = await sessionBridge.bind({
				plotId: result.envelope.plot.plot_id,
				sessionId: selectedSessionId,
			}, ctx.sessionManager);
			if (!endpoint) {
				notify(ctx, "Plot was established, but its authoritative Session bridge could not be started", "error");
				return false;
			}
			result = await establishPlot(exec, {
				event: boundary.event,
				cwd: ctx.cwd,
				protocol: endpoint,
			});
			if (!result.ok) {
				notify(ctx, result.error, "error");
				return false;
			}
		} else {
			notify(ctx, "Plot established without a selected Session; structured protocol was not attached", "warning");
		}
		pi.appendEntry<EstablishmentAppliedEntry>(ESTABLISHMENT_APPLIED_ENTRY, {
			runId: run.runId,
			boundary,
			plotId: result.envelope.plot.plot_id,
			establishedAt: new Date().toISOString(),
		});
		completeEstablishmentRun(run.runId);
		notify(ctx, `Plot ${result.envelope.outcome} from ${boundary.event}`, "success");
		return true;
	};
	const recoverEstablishments = async (ctx: ExtensionContext) => {
		const runs = unresolvedEstablishmentRuns(ctx);
		if (runs.length === 0) return;
		const config = await configCache.get(ctx.cwd);
		if (!config.available) return;
		const after = await readLatestCheckpoint(exec, ctx.cwd);
		if (!after) return;
		for (const run of runs) {
			const boundary = pickNewBoundary(run.before, after, config.establishmentEvents, new Set(), new Set());
			if (boundary) {
				await applyEstablishment(run, boundary, ctx);
				continue;
			}
			const anyBoundary = pickNewBoundary(run.before, after, "all", new Set(), new Set());
			if (anyBoundary) completeEstablishmentRun(run.runId);
		}
	};

	pi.on("session_start", async (_event, ctx) => {
		refreshConsumed(ctx, consumed);
		configCache.refresh();
		await recoverEstablishments(ctx);

		// Hide the native `/skill:<name>` picker entry for every beislid skill this
		// extension already wraps with its own command (e.g. `/kickoff` vs.
		// `/skill:kickoff`), without touching a user's own skills.
		//
		// `pi.registerCommand` cannot substitute for this: it can only *add* a new
		// command, never remove or hide pi's own `skill:<name>` picker entries (those
		// come straight from the resource loader, not from any extension's command
		// map - see `dist/core/agent-session.js`'s `_bindExtensionCore`). The
		// documented seam for reshaping picker suggestions is
		// `ctx.ui.addAutocompleteProvider` (see `docs/extensions.md`, "Autocomplete
		// Providers"), which layers a wrapper in front of the built-in provider.
		//
		// `ctx.ui.addAutocompleteProvider` needs `ctx.hasUI` (interactive/TUI mode);
		// it is registered here, in `session_start`, per the documented pattern,
		// because it must run after `ctx.ui` exists and it is re-applied on every
		// rebind (fork/switch/reload all re-emit `session_start` and rebuild the
		// autocomplete stack - see `interactive-mode.js`'s `bindCurrentSessionExtensions`
		// calling `setupAutocompleteProvider()` right after emitting `session_start`).
		if (ctx.hasUI) {
			ctx.ui.addAutocompleteProvider((current) => ({
				async getSuggestions(lines, line, col, options) {
					const result = await current.getSuggestions(lines, line, col, options);
					if (!result) return result;
					return { ...result, items: filterWrappedSkillAutocompleteItems(result.items) };
				},
				applyCompletion: (lines, line, col, item, prefix) => current.applyCompletion(lines, line, col, item, prefix),
				shouldTriggerFileCompletion: (lines, line, col) => current.shouldTriggerFileCompletion?.(lines, line, col) ?? true,
			}));
		}
	});

	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName !== "bash") return;
		const command = (event as { input?: { command?: unknown } }).input?.command;
		if (typeof command === "string") workflowSignals.surfaceWorkflowSignalsFromCommand(ctx, command);
	});

	for (const skill of NOPAL_SKILLS) {
		const command = commandNameForSkill(skill);
		pi.registerCommand(command, {
			description: `Run the Nopal ${skill} skill through the managed Pi wrapper`,
			handler: async (args, ctx) => {
				if (skill === "babysit") {
					workflowSignals.emitWorkflowSignal(ctx, { state: "working", skill, phase: "runtime" });
					await babysitRuntime.start(args, ctx);
					return;
				}

				const initialSignal = initialSignalForSkill(skill);
				if (initialSignal) workflowSignals.emitWorkflowSignal(ctx, initialSignal);
				const prompt = skillPrompt(skill, args);
				if (BOUNDARY_CAPABLE_SKILLS.has(skill)) {
					await recoverEstablishments(ctx);
					const run: ManagedRun = {
						runId: `${Date.now()}-${++runSequence}`,
						skill,
						command,
						startedAt: Date.now(),
						before: await readLatestCheckpoint(exec, ctx.cwd),
					};
					activeRun = run;
					pi.appendEntry<ManagedRun>(ESTABLISHMENT_PENDING_ENTRY, run);
				}
				await pi.sendUserMessage(prompt, { deliverAs: "followUp" });
			},
		});
	}

	pi.registerCommand(INTERNAL_HANDOFF_COMMAND, {
		description: "Internal Nopal command: start a fresh Pi session from the latest checkpoint boundary",
		handler: async (_args, ctx) => {
			const boundary = pendingBoundary;
			const workflow = pendingWorkflow ?? "managed";
			pendingBoundary = undefined;
			pendingWorkflow = undefined;
			if (!boundary) return;

			if (ctx.mode === "print" || ctx.mode === "json") {
				notify(ctx, "Nopal auto-handoff skipped in this Pi mode; use the checkpoint pointer manually.", "warning");
				return;
			}

			consumed.add(boundary.id);
			pi.appendEntry<ConsumedEntry>(CONSUMED_ENTRY, { boundary, consumedAt: new Date().toISOString() });
			notify(ctx, `Starting fresh Pi session from checkpoint ${boundary.path}`, "info");
			const parentSession = ctx.sessionManager.getSessionFile();
			const prompt = continuationPrompt(boundary, workflow);
			const result = await ctx.newSession({
				parentSession,
				setup: async (sessionManager) => {
					sessionManager.appendCustomEntry(CONSUMED_ENTRY, { boundary, consumedAt: new Date().toISOString() });
				},
				withSession: async (replacementCtx) => {
					await replacementCtx.sendUserMessage(prompt);
				},
			});
			if (result.cancelled) notify(ctx, "Nopal auto-handoff cancelled by Pi session guard.", "warning");
		},
	});

	pi.on("agent_end", async (_event, ctx) => {
		if (!activeRun) return;
		const run = activeRun;
		activeRun = undefined;
		refreshConsumed(ctx, consumed);
		const config = await configCache.get(ctx.cwd);
		if (!config.available) return;
		const after = await readLatestCheckpoint(exec, ctx.cwd);
		const establishmentBoundary = pickNewBoundary(
			run.before,
			after,
			config.establishmentEvents,
			new Set(),
			consumed,
		);
		if (establishmentBoundary) {
			if (!await applyEstablishment(run, establishmentBoundary, ctx)) return;
		} else {
			completeEstablishmentRun(run.runId);
		}
		if (!config.handoff.autoHandoff) return;
		const allowedEvents = config.handoff.events;
		const boundary = pickNewBoundary(run.before, after, allowedEvents, config.handoff.exclude, consumed);
		if (!boundary) return;
		pendingBoundary = boundary;
		pendingWorkflow = run.skill;
		await pi.sendUserMessage(`/${INTERNAL_HANDOFF_COMMAND}`, { deliverAs: "followUp" });
	});
}
