import path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { classifyBashCommand, redactToolContent, shouldBlockProtectedCredentialPath } from "./classifier.js";
import { planEnforcement, recordEnforcementGate, resolvePolicyMode, type EnforcementGate } from "./nopal-cli.js";

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

function buildStatusLines(mode: string, stats: GateStats): string[] {
	return [
		"Nopal enforcement: ON for the entire session",
		`Mode (NOPAL_POLICY_MODE): ${mode}`,
		"Protected floors: credential-path blocking, secret redaction, and internal enforcement API blocking",
		`Decisions this session: ${stats.total} (allowed ${stats.allowed}, denied ${stats.denied}, asked ${stats.asked}, approved ${stats.approved}, blocked ${stats.blocked}, fail-closed ${stats.failClosed})`,
	];
}

function gateWorkingDirectory(root: string, configured: string | undefined): string | undefined {
	const resolvedRoot = path.resolve(root);
	const resolved = path.resolve(resolvedRoot, configured ?? ".");
	return resolved === resolvedRoot || resolved.startsWith(`${resolvedRoot}${path.sep}`) ? resolved : undefined;
}

async function executeGate(pi: ExtensionAPI, gate: EnforcementGate, root: string) {
	const cwd = gateWorkingDirectory(root, gate.cwd);
	if (!cwd) return { stdout: "", stderr: "gate cwd escapes repository root", code: 2 };
	if ("command" in gate.run) return pi.exec("bash", ["-lc", gate.run.command], { cwd });
	const [command, ...args] = gate.run.argv;
	if (!command) return { stdout: "", stderr: "gate argv is empty", code: 2 };
	return pi.exec(command, args, { cwd });
}

export default function policyGate(pi: ExtensionAPI) {
	const stats = createStats();

	pi.registerCommand("policy-gate", {
		description: "Show the always-on Nopal enforcement status",
		handler: async (args, ctx) => {
			const token = args.trim().toLowerCase();
			if (token && token !== "status") {
				notify(ctx, "Usage: /policy-gate [status]", "warning");
				return;
			}
			notify(ctx, buildStatusLines(resolvePolicyMode(process.env), stats).join("\n"), "info");
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

		const command = String((event.input as { command?: unknown }).command ?? "");
		if (!command.trim()) return undefined;

		const classification = classifyBashCommand(command);
		if (classification.action === "nopal.enforcement_internal") {
			stats.blocked += 1;
			return { block: true, reason: "The enforcement machine API is reserved for the trusted Pi adapter" };
		}
		const runId = process.env.NOPAL_ENFORCEMENT_RUN_ID;
		if (!runId) {
			stats.failClosed += 1;
			stats.blocked += 1;
			return { block: true, reason: "Nopal enforcement was not initialized for this Pi session" };
		}
		const mode = resolvePolicyMode(process.env);
		const params = {
			mode,
			action: classification.action,
			class: classification.class,
			runId,
			cwd: ctx.cwd,
		};
		let result = await planEnforcement((cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options), params);

		stats.total += 1;
		if (result.failClosed) {
			stats.failClosed += 1;
			stats.blocked += 1;
			return { block: true, reason: result.explanation };
		}

		if (result.decision === "deny") {
			stats.denied += 1;
			return { block: true, reason: result.explanation };
		}

		if (result.decision === "ask") {
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
		}

		for (const gate of result.requiredGates) {
			const gateResult = await executeGate(pi, gate, result.root);
			const recorded = await recordEnforcementGate(
				(cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options),
				{ ...params, gateId: gate.id, exitCode: gateResult.code },
			);
			if (!recorded) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: `Could not durably record gate ${gate.id}` };
			}
			if (gateResult.code !== 0) {
				stats.blocked += 1;
				return { block: true, reason: `Required gate ${gate.id} failed: ${gateResult.stderr}` };
			}
		}

		if (result.requiredGates.length > 0) {
			result = await planEnforcement((cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options), params);
			if (result.failClosed || result.decision !== "allow" || result.requiredGates.length > 0) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Gate receipts were not current after execution; blocking fail closed" };
			}
		}
		stats.allowed += 1;
		return undefined;
	});

	pi.on("tool_result", async (event) => {
		if (event.toolName !== "bash" && event.toolName !== "read") return undefined;
		return { content: redactToolContent(event.content) as never };
	});
}
