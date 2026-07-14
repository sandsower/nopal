import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { classifyBashCommand, redactToolContent, shouldBlockProtectedCredentialPath } from "./classifier.js";
import { decidePolicy, resolvePolicyMode, type PolicyDecisionResult } from "./nopal-cli.js";

const STATE_ENTRY_TYPE = "policy-gate-state";

type PolicyGateStateEntry = { data?: { enabled?: unknown } };

type GateStats = {
	total: number;
	allowed: number;
	denied: number;
	asked: number;
	approved: number;
	blocked: number;
	failClosed: number;
};

function createStats(): GateStats {
	return { total: 0, allowed: 0, denied: 0, asked: 0, approved: 0, blocked: 0, failClosed: 0 };
}

function notify(ctx: ExtensionContext, message: string, level: "info" | "warning" | "error" = "info"): void {
	if (ctx.hasUI) ctx.ui.notify(message, level);
}

/** pi's own read-only tools; distinct from bash shell commands that happen to be read-only. */
function isSkippedReadOnlyTool(toolName: string): boolean {
	return toolName === "read" || toolName === "grep" || toolName === "find" || toolName === "ls";
}

function buildStatusLines(enabled: boolean, mode: string, stats: GateStats, cacheSize: number): string[] {
	return [
		`Policy gate: ${enabled ? "ON" : "OFF"}`,
		`Mode (NOPAL_POLICY_MODE): ${mode}`,
		"Protected floors: always ON (credential-path blocking for write/edit, secret redaction of bash/read results)",
		`Decisions this session: ${stats.total} (allowed ${stats.allowed}, denied ${stats.denied}, asked ${stats.asked}, approved ${stats.approved}, blocked ${stats.blocked}, fail-closed ${stats.failClosed})`,
		`Cached commands: ${cacheSize}`,
		"",
		"Commands: /policy-gate status | on | off",
	];
}

export default function policyGate(pi: ExtensionAPI) {
	let enabled = true;
	const stats = createStats();
	const decisionCache = new Map<string, PolicyDecisionResult>();

	function persistState(): void {
		pi.appendEntry(STATE_ENTRY_TYPE, { enabled });
	}

	pi.on("session_start", async (_event, ctx) => {
		const stateEntry = ctx.sessionManager
			.getEntries()
			.filter((entry: { type: string; customType?: string }) => entry.type === "custom" && entry.customType === STATE_ENTRY_TYPE)
			.pop() as PolicyGateStateEntry | undefined;
		if (typeof stateEntry?.data?.enabled === "boolean") enabled = stateEntry.data.enabled;
	});

	pi.registerCommand("policy-gate", {
		description: "Show policy-gate status; use /policy-gate on|off to toggle the Nopal policy roundtrip (protected floors always stay on)",
		handler: async (args, ctx) => {
			const token = args.trim().toLowerCase();
			if (token === "on" || token === "off") {
				enabled = token === "on";
				persistState();
				notify(ctx, `Policy gate ${enabled ? "enabled" : "disabled"}. Protected floors remain active.`, "info");
				return;
			}
			if (token && token !== "status") {
				notify(ctx, "Usage: /policy-gate [status|on|off]", "warning");
				return;
			}
			const mode = resolvePolicyMode(process.env);
			notify(ctx, buildStatusLines(enabled, mode, stats, decisionCache.size).join("\n"), "info");
		},
	});

	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName === "write" || event.toolName === "edit") {
			const path = String((event.input as { path?: unknown }).path ?? "");
			if (shouldBlockProtectedCredentialPath(event.toolName, path)) {
				return { block: true, reason: `Protected credential path: ${path}` };
			}
			return undefined;
		}

		if (isSkippedReadOnlyTool(event.toolName)) return undefined;
		if (event.toolName !== "bash") return undefined;
		if (!enabled) return undefined;

		const command = String((event.input as { command?: unknown }).command ?? "");
		if (!command.trim()) return undefined;

		let result = decisionCache.get(command);
		if (!result) {
			const classification = classifyBashCommand(command);
			const mode = resolvePolicyMode(process.env);
			result = await decidePolicy((cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options), {
				mode,
				action: classification.action,
				class: classification.class,
				cwd: ctx.cwd,
			});
			decisionCache.set(command, result);
		}

		stats.total += 1;
		if (result.failClosed) stats.failClosed += 1;

		if (result.decision === "allow") {
			stats.allowed += 1;
			return undefined;
		}

		if (result.decision === "deny") {
			stats.denied += 1;
			return { block: true, reason: result.explanation };
		}

		// ask
		stats.asked += 1;
		if (!ctx.hasUI) {
			stats.blocked += 1;
			return { block: true, reason: `${result.explanation} (no UI available for confirmation)` };
		}

		const choice = await ctx.ui.select(`Policy gate: ${result.explanation}\n\n${command}\n\nAllow this command?`, ["No, block it", "Yes, run it"]);
		if (choice !== "Yes, run it") {
			stats.blocked += 1;
			return { block: true, reason: "Blocked by user" };
		}
		stats.approved += 1;
		return undefined;
	});

	pi.on("tool_result", async (event) => {
		if (event.toolName !== "bash" && event.toolName !== "read") return undefined;
		return { content: redactToolContent(event.content) as never };
	});
}
