import { realpathSync } from "node:fs";
import path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import {
	classifyBashCommandSet,
	commandReferencesEnforcementAuthority,
	isProtectedEnforcementPath,
	redactToolContent,
	shouldBlockProtectedCredentialPath,
	type EnforcementAuthority,
} from "./classifier.js";
import {
	planEnforcement,
	reauthorizationIsCurrent,
	recordEnforcementGate,
	resolvePolicyMode,
	type EnforcementGate,
} from "./nopal-cli.js";

type EnforcementBootstrap = {
	authority: EnforcementAuthority;
};

const BOOTSTRAP_PROPERTY = "__nopalEnforcementBootstrapV1";

function loadBootstrap(): EnforcementBootstrap | undefined {
	const host = globalThis as unknown as Record<string, unknown>;
	const retained = host[BOOTSTRAP_PROPERTY];
	if (retained && typeof retained === "object") return retained as EnforcementBootstrap;

	const runId = process.env.NOPAL_ENFORCEMENT_RUN_ID;
	const projectRoot = process.env.NOPAL_ENFORCEMENT_ROOT;
	const stateDir = process.env.NOPAL_ENFORCEMENT_STATE_DIR;
	const configDir = process.env.NOPAL_ENFORCEMENT_CONFIG_DIR;
	const adapterDir = process.env.NOPAL_ENFORCEMENT_ADAPTER_DIR;
	const nopalBin = process.env.NOPAL_ENFORCEMENT_CLI;
	delete process.env.NOPAL_ENFORCEMENT_RUN_ID;
	delete process.env.NOPAL_ENFORCEMENT_ROOT;
	delete process.env.NOPAL_ENFORCEMENT_STATE_DIR;
	delete process.env.NOPAL_ENFORCEMENT_CONFIG_DIR;
	delete process.env.NOPAL_ENFORCEMENT_ADAPTER_DIR;
	delete process.env.NOPAL_ENFORCEMENT_CLI;
	if (!runId || !projectRoot || !stateDir || !adapterDir || !nopalBin) return undefined;

	const bootstrap: EnforcementBootstrap = {
		authority: { runId, projectRoot, stateDir, adapterDir, nopalBin, ...(configDir ? { configDir } : {}) },
	};
	Object.defineProperty(host, BOOTSTRAP_PROPERTY, {
		value: bootstrap,
		enumerable: false,
		writable: false,
		configurable: false,
	});
	return bootstrap;
}

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
	const bootstrap = loadBootstrap();
	const authority = bootstrap?.authority;

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
		const inputPath = String((event.input as { path?: unknown }).path ?? "");
		if (inputPath && authority) {
			let protectedPath = isProtectedEnforcementPath(inputPath, ctx.cwd, authority);
			try {
				protectedPath ||= isProtectedEnforcementPath(realpathSync(path.resolve(ctx.cwd, inputPath)), ctx.cwd, authority);
			} catch {
				// A not-yet-created path still receives lexical protection above.
			}
			if (protectedPath) {
				stats.blocked += 1;
				return { block: true, reason: "Nopal enforcement authority is not accessible to agent tools" };
			}
		}

		if (event.toolName === "write" || event.toolName === "edit") {
			if (shouldBlockProtectedCredentialPath(event.toolName, inputPath)) {
				return { block: true, reason: `Protected credential path: ${inputPath}` };
			}
			return undefined;
		}

		if (isSkippedReadOnlyTool(event.toolName)) return undefined;
		if (event.toolName !== "bash") return undefined;

		const command = String((event.input as { command?: unknown }).command ?? "");
		if (!command.trim()) return undefined;
		if (!authority) {
			stats.failClosed += 1;
			stats.blocked += 1;
			return { block: true, reason: "Nopal enforcement was not initialized for this Pi session" };
		}
		if (commandReferencesEnforcementAuthority(command, ctx.cwd, authority)) {
			stats.blocked += 1;
			return { block: true, reason: "The enforcement contract and evidence store are reserved for the trusted Pi adapter" };
		}

		const commandClassifications = classifyBashCommandSet(command);
		if (!commandClassifications.complete) {
			stats.failClosed += 1;
			stats.blocked += 1;
			return { block: true, reason: commandClassifications.reason ?? "The shell command could not be classified completely" };
		}
		if (commandClassifications.classifications.some((classification) => classification.action === "nopal.enforcement_internal")) {
			stats.blocked += 1;
			return { block: true, reason: "The enforcement machine API is reserved for the trusted Pi adapter" };
		}

		const mode = resolvePolicyMode(process.env);
		const planned = [] as Array<{
			params: { mode: string; action: string; class: string; runId: string; nopalBin: string; cwd: string };
			result: Awaited<ReturnType<typeof planEnforcement>>;
			approved: boolean;
		}>;
		for (const classification of commandClassifications.classifications) {
			const params = {
				mode,
				action: classification.action,
				class: classification.class,
				runId: authority.runId,
				nopalBin: authority.nopalBin,
				cwd: ctx.cwd,
			};
			const result = await planEnforcement((cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options), params);
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
			let approved = false;
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
				approved = true;
				stats.approved += 1;
			}
			planned.push({ params, result, approved });
		}

		for (const action of planned) {
			const initialContract = action.result.contractDigest;
			const initialWorkspace = action.result.workspaceFingerprint;
			for (const gate of action.result.requiredGates) {
				const gateResult = await executeGate(pi, gate, action.result.root);
				const recorded = await recordEnforcementGate(
					(cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options),
					{
						...action.params,
						gateId: gate.id,
						exitCode: gateResult.code,
						contractDigest: initialContract,
						workspaceFingerprint: initialWorkspace,
						gateDefinitionDigest: gate.definitionDigest,
					},
				);
				if (!recorded) {
					stats.failClosed += 1;
					stats.blocked += 1;
					return { block: true, reason: `Could not durably record gate ${gate.id}; enforcement context changed or evidence authentication failed` };
				}
				if (gateResult.code !== 0) {
					stats.blocked += 1;
					return { block: true, reason: `Required gate ${gate.id} failed: ${gateResult.stderr}` };
				}
			}

			if (action.result.requiredGates.length > 0) {
				const reauthorized = await planEnforcement(
					(cmd, cmdArgs, options) => pi.exec(cmd, cmdArgs, options),
					action.params,
				);
				if (!reauthorizationIsCurrent(action.result, reauthorized, action.approved)) {
					stats.failClosed += 1;
					stats.blocked += 1;
					return { block: true, reason: "Gate receipts or the approved authorization context were not current after execution; blocking fail closed" };
				}
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
