import { spawn } from "node:child_process";
import { closeSync, mkdtempSync, openSync, realpathSync, rmSync, unlinkSync, writeSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import {
	classifyPiToolCall,
	commandReferencesEnforcementAuthority,
	isProtectedEnforcementPath,
	redactToolContent,
	shouldBlockProtectedCredentialPath,
	type EnforcementAuthority,
} from "./classifier.ts";
import {
	advanceEnforcement,
	cleanupEnforcementRuntime,
	closeEnforcementRun,
	planEnforcement,
	recordEnforcementApproval,
	recordEnforcementOutcome,
	type EnforcementGate,
	type EnforcementParams,
	type ExecFn,
} from "./nopal-cli.ts";

export const EXPECTED_PI_TOOL_CATALOG = ["bash", "edit", "find", "grep", "ls", "read", "write"] as const;

export function activeToolCatalogIsExpected(activeTools: readonly string[]): boolean {
	const unique = [...new Set(activeTools)].sort();
	return unique.length === EXPECTED_PI_TOOL_CATALOG.length
		&& unique.every((tool, index) => tool === EXPECTED_PI_TOOL_CATALOG[index]);
}

export type GuardStats = {
	total: number;
	allowed: number;
	denied: number;
	asked: number;
	approved: number;
	blocked: number;
	failClosed: number;
};

export function createGuardStats(): GuardStats {
	return { total: 0, allowed: 0, denied: 0, asked: 0, approved: 0, blocked: 0, failClosed: 0 };
}

function runBoundedProcess(
	command: string,
	args: string[],
	options: { capability?: string; cwd?: string; env: NodeJS.ProcessEnv; input?: string; timeout: number },
): ReturnType<ExecFn> {
	return new Promise((resolve, reject) => {
		let capabilityFd: number | undefined;
		if (options.capability !== undefined) {
			const directory = mkdtempSync(path.join(tmpdir(), "nopal-capability-"));
			const capabilityPath = path.join(directory, "channel");
			capabilityFd = openSync(capabilityPath, "w+", 0o600);
			unlinkSync(capabilityPath);
			rmSync(directory, { recursive: true });
			const bytes = Buffer.from(options.capability, "utf8");
			if (writeSync(capabilityFd, bytes, 0, bytes.length, 0) !== bytes.length) {
				closeSync(capabilityFd);
				reject(new Error("private enforcement capability channel write was incomplete"));
				return;
			}
		}
		let child: ReturnType<typeof spawn>;
		try {
			child = spawn(command, args, {
				cwd: options.cwd,
				detached: true,
				env: options.env,
				stdio: capabilityFd === undefined
					? ["pipe", "pipe", "pipe"]
					: ["pipe", "pipe", "pipe", capabilityFd],
			});
		} finally {
			if (capabilityFd !== undefined) closeSync(capabilityFd);
		}
		const killProcessGroup = () => {
			try {
				if (child.pid) process.kill(-child.pid, "SIGKILL");
				else child.kill("SIGKILL");
			} catch {
				child.kill("SIGKILL");
			}
		};
		const stdout: Buffer[] = [];
		const stderr: Buffer[] = [];
		let outputBytes = 0;
		let killedForLimit = false;
		const collect = (chunks: Buffer[], chunk: Buffer) => {
			outputBytes += chunk.length;
			if (outputBytes > 1024 * 1024) {
				killedForLimit = true;
				killProcessGroup();
				return;
			}
			chunks.push(chunk);
		};
		child.stdout.on("data", (chunk: Buffer) => collect(stdout, chunk));
		child.stderr.on("data", (chunk: Buffer) => collect(stderr, chunk));
		child.on("error", reject);
		const input = options.input ?? "";
		child.stdin.on("error", (error: NodeJS.ErrnoException) => {
			// A command that consumes no stdin may exit before Node flushes the
			// empty stream. Private Core requests carry non-empty authenticated
			// input, so their write failure remains fatal.
			if (input.length > 0 || error.code !== "EPIPE") reject(error);
		});
		const timer = setTimeout(killProcessGroup, options.timeout);
		child.on("close", (code, signal) => {
			clearTimeout(timer);
			if (killedForLimit) {
				reject(new Error("subprocess response exceeded one MiB"));
				return;
			}
			resolve({
				stdout: Buffer.concat(stdout).toString("utf8"),
				stderr: Buffer.concat(stderr).toString("utf8"),
				code: code ?? 128,
				killed: signal !== null,
			});
		});
		child.stdin.end(input);
	});
}

function privateCoreExec(authority: EnforcementAuthority): ExecFn {
	return (command, args, options = {}) => {
		if (command !== authority.nopalBin) {
			return Promise.reject(new Error("private enforcement transport rejected a substituted executable"));
		}
		return runBoundedProcess(command, args, {
			cwd: options.cwd,
			capability: authority.adapterCapability,
			env: {
				...process.env,
				BEISLID_STATE_DIR: authority.stateDir,
				NOPAL_ENFORCEMENT_CAPABILITY_FD: "3",
				...(authority.configDir ? { NOPAL_CONFIG_DIR: authority.configDir } : {}),
			},
			input: options.input,
			timeout: options.timeout ?? 10_000,
		});
	};
}

function gateAllowsParallelRead(gate: EnforcementGate): boolean {
	return gate.parallelSafe === true && gate.mutates === false && gate.autofix === undefined;
}

function protectedInputPath(event: { input: Record<string, unknown> }): string {
	return String(event.input.path ?? "");
}

function pathTargetsAuthority(inputPath: string, cwd: string, authority: EnforcementAuthority): boolean {
	if (!inputPath) return false;
	let protectedPath = isProtectedEnforcementPath(inputPath, cwd, authority);
	try {
		protectedPath ||= isProtectedEnforcementPath(realpathSync(path.resolve(cwd, inputPath)), cwd, authority);
	} catch {
		// A new path still receives lexical and nearest-existing-ancestor checks.
	}
	return protectedPath;
}

function placementIsSatisfied(placement: string, _ctx: ExtensionContext, _authority: EnforcementAuthority): boolean {
	// A direct Nopal launch proves only that this process is the active user
	// runtime for the repository. Dedicated repository or run placement needs
	// signed evidence from a trusted placement adapter, which this launcher
	// deliberately does not fabricate.
	return placement === "shared_user_runtime";
}

function sessionIdentity(ctx: ExtensionContext): string {
	return ctx.sessionManager.getSessionFile() ?? "ephemeral-session";
}

function toolResultWasCancelled(event: { isError: boolean; content: unknown }): boolean {
	if (!event.isError || !Array.isArray(event.content)) return false;
	return event.content.some((part) => {
		if (!part || typeof part !== "object") return false;
		const text = (part as { type?: unknown; text?: unknown }).type === "text"
			? (part as { text?: unknown }).text
			: undefined;
		return typeof text === "string"
			&& (text === "Operation aborted"
				|| text === "Command aborted"
				|| text.endsWith("\n\nCommand aborted"));
	});
}

export function installPiActionGuard(
	pi: ExtensionAPI,
	authority: EnforcementAuthority | undefined,
	mode: string,
	stats: GuardStats,
	injectedCoreExec?: ExecFn,
): void {
	type ActiveRelease = {
		toolCallId: string;
		params: EnforcementParams;
		authorizationBinding: string;
		releaseId: string;
	};
	type ActiveCall = {
		mutates: boolean;
		release?: ActiveRelease;
	};
	const activeCalls = new Map<string, ActiveCall>();
	let activeMutatingCallId: string | undefined;
	let concurrencyPoisoned = false;
	let shuttingDown = false;
	const coreExec = authority
		? (injectedCoreExec ?? privateCoreExec(authority))
		: injectedCoreExec;

	const removeActiveCall = (toolCallId: string) => {
		const active = activeCalls.get(toolCallId);
		if (active?.mutates && activeMutatingCallId === toolCallId) activeMutatingCallId = undefined;
		activeCalls.delete(toolCallId);
	};

	pi.on("tool_call", async (event, ctx) => {
		if (concurrencyPoisoned || shuttingDown) {
			stats.blocked += 1;
			return {
				block: true,
				reason: "Nopal authorization is unavailable until session shutdown completes after a concurrent or unrecordable tool event",
			};
		}
		let leaseAcquired = false;
		let releaseOnReturn = true;
		try {
			if (!authority) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Nopal enforcement was not initialized for this Pi session" };
			}

			const inputPath = protectedInputPath(event as { input: Record<string, unknown> });
			if (pathTargetsAuthority(inputPath, ctx.cwd, authority)) {
				stats.blocked += 1;
				return { block: true, reason: "Nopal enforcement authority is not accessible to agent tools" };
			}
			if (shouldBlockProtectedCredentialPath(event.toolName, inputPath)) {
				stats.blocked += 1;
				return { block: true, reason: `Protected credential path: ${inputPath}` };
			}

			const compiled = classifyPiToolCall(
				event.toolName,
				event.input as Record<string, unknown>,
				ctx.cwd,
				authority.projectRoot,
			);
			if (!compiled.complete || !compiled.intent) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: compiled.reason ?? "Pi tool call could not be classified completely" };
			}
			const intent = compiled.intent;
			if (event.toolName === "bash") {
				const command = String((event.input as { command?: unknown }).command ?? "");
				if (commandReferencesEnforcementAuthority(command, ctx.cwd, authority)) {
					stats.blocked += 1;
					return { block: true, reason: "The enforcement contract and evidence store are reserved for the trusted Pi adapter" };
				}
				if (intent.action === "nopal.enforcement_internal") {
					stats.blocked += 1;
					return { block: true, reason: "The enforcement machine API is reserved for the trusted Pi adapter" };
				}
			}

			const duplicate = activeCalls.has(event.toolCallId);
			const conflicts = intent.mutates ? activeCalls.size > 0 : activeMutatingCallId !== undefined;
			if (duplicate || conflicts) {
				concurrencyPoisoned = true;
				stats.blocked += 1;
				return {
					block: true,
					reason: duplicate
						? "Nopal rejected a duplicate in-flight Pi tool-call identity"
						: "Nopal rejected an overlapping read/mutation or mutation/mutation batch",
				};
			}
			activeCalls.set(event.toolCallId, { mutates: intent.mutates });
			if (intent.mutates) activeMutatingCallId = event.toolCallId;
			leaseAcquired = true;

			const params = {
				mode,
				action: intent.action,
				class: intent.class,
				runId: authority.runId,
				nopalBin: authority.nopalBin,
				adapterCapability: authority.adapterCapability,
				launchId: authority.runId,
				sessionId: sessionIdentity(ctx),
				toolCallId: event.toolCallId,
				toolName: event.toolName,
				inputDigest: intent.inputDigest,
				targetDigest: intent.targetDigest,
				executorDigest: authority.gateExecutorDigest,
				runtimeDigest: authority.gateRuntimeDigest,
				changedFiles: intent.changedFiles,
				mutates: intent.mutates,
				cwd: ctx.cwd,
			};
			if (!coreExec) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Nopal private enforcement transport was not initialized" };
			}
			const result = await planEnforcement(coreExec, params);
			stats.total += 1;
			if (result.failClosed) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: result.explanation };
			}
			try {
				if (realpathSync(result.root) !== realpathSync(authority.projectRoot)) {
					stats.failClosed += 1;
					stats.blocked += 1;
					return { block: true, reason: "Core authorization returned a foreign repository root" };
				}
			} catch {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Core authorization repository identity could not be verified" };
			}
			if (!placementIsSatisfied(result.placement, ctx, authority)) {
				stats.blocked += 1;
				return { block: true, reason: `${result.explanation}; required placement is not established for this run` };
			}
			if (result.decision === "deny") {
				stats.denied += 1;
				return { block: true, reason: result.explanation };
			}

			const requiresExclusiveLease = !intent.mutates
				&& (result.decision === "ask" || !result.requiredGates.every(gateAllowsParallelRead));
			if (requiresExclusiveLease) {
				const active = activeCalls.get(event.toolCallId);
				if (!active || activeCalls.size !== 1 || activeMutatingCallId !== undefined) {
					stats.blocked += 1;
					return {
						block: true,
						reason: "Nopal could not acquire an exclusive lease for a workspace-sensitive gate or approval",
					};
				}
				active.mutates = true;
				activeMutatingCallId = event.toolCallId;
			}

			let advanced = await advanceEnforcement(coreExec, params);
			if (advanced.failClosed) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: advanced.reason ?? advanced.plan.explanation };
			}
			let current = advanced.plan;
			try {
				if (realpathSync(current.root) !== realpathSync(authority.projectRoot)) {
					stats.failClosed += 1;
					stats.blocked += 1;
					return { block: true, reason: "Verification transaction returned a foreign repository root" };
				}
			} catch {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Verification transaction repository identity could not be verified" };
			}
			if (!placementIsSatisfied(current.placement, ctx, authority)) {
				stats.blocked += 1;
				return { block: true, reason: `${current.explanation}; required placement is not established for this run` };
			}
			if (advanced.state === "blocked") {
				if (current.decision === "deny") stats.denied += 1;
				stats.blocked += 1;
				return { block: true, reason: advanced.reason ?? current.explanation };
			}

			if (advanced.state === "approval_required") {
				stats.asked += 1;
				if (!ctx.hasUI) {
					stats.blocked += 1;
					return { block: true, reason: `${current.explanation} (no UI available for confirmation)` };
				}
				const choice = await ctx.ui.select(
					`Policy gate: ${current.explanation}\n\n${event.toolName} ${JSON.stringify(event.input)}\n\nAllow this exact tool call?`,
					["No, block it", "Yes, run it"],
				);
				const approved = choice === "Yes, run it";
				const recorded = await recordEnforcementApproval(
					coreExec,
					{ ...params, authorizationBinding: current.authorizationBinding, approved },
				);
				if (!recorded || !approved) {
					stats.blocked += 1;
					return { block: true, reason: approved ? "Could not durably record approval" : "Blocked by user" };
				}
				stats.approved += 1;
				advanced = await advanceEnforcement(coreExec, params);
				current = advanced.plan;
				if (advanced.failClosed
					|| advanced.state !== "released"
					|| !current.approvalCurrent
					|| !current.authorized) {
					stats.failClosed += 1;
					stats.blocked += 1;
					return { block: true, reason: "Durable approval did not match the current exact authorization subject" };
				}
			}

			if (advanced.state !== "released" || !advanced.releaseId || !current.authorized) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Core did not durably release this exact Pi tool call" };
			}
			const releaseId = advanced.releaseId;
			const active = activeCalls.get(event.toolCallId);
			if (!active) {
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "The active call lease vanished before exact authorization release" };
			}
			const release = {
				toolCallId: event.toolCallId,
				params,
				authorizationBinding: current.authorizationBinding,
				releaseId,
			};
			active.release = release;
			if (shuttingDown) {
				const interrupted = await recordEnforcementOutcome(coreExec, {
					...release.params,
					authorizationBinding: release.authorizationBinding,
					releaseId: release.releaseId,
					outcome: "interrupted",
				});
				releaseOnReturn = false;
				if (interrupted) removeActiveCall(event.toolCallId);
				else concurrencyPoisoned = true;
				stats.failClosed += 1;
				stats.blocked += 1;
				return { block: true, reason: "Session shutdown raced the exact authorization release" };
			}

			stats.allowed += 1;
			releaseOnReturn = false;
			return undefined;
		} finally {
			if (releaseOnReturn && leaseAcquired) removeActiveCall(event.toolCallId);
		}
	});

	pi.on("tool_result", async (event) => {
		const active = activeCalls.get(event.toolCallId);
		const release = active?.release;
		if (!release || !coreExec || release.toolCallId !== event.toolCallId) {
			concurrencyPoisoned = true;
			stats.failClosed += 1;
			stats.blocked += 1;
			return { content: redactToolContent(event.content) as never };
		}
		const recorded = await recordEnforcementOutcome(coreExec, {
			...release.params,
			authorizationBinding: release.authorizationBinding,
			releaseId: release.releaseId,
			outcome: toolResultWasCancelled(event)
				? "cancelled"
				: event.isError ? "error" : "success",
		});
		if (!recorded) {
			concurrencyPoisoned = true;
			stats.failClosed += 1;
			stats.blocked += 1;
		} else {
			removeActiveCall(event.toolCallId);
		}
		return { content: redactToolContent(event.content) as never };
	});

	pi.on("session_shutdown", async () => {
		shuttingDown = true;
		const runWasInterrupted = activeCalls.size > 0 || concurrencyPoisoned;
		let interruptionRecorded = true;
		for (const active of activeCalls.values()) {
			const release = active.release;
			if (!release) continue;
			if (!coreExec || !await recordEnforcementOutcome(coreExec, {
				...release.params,
				authorizationBinding: release.authorizationBinding,
				releaseId: release.releaseId,
				outcome: "interrupted",
			})) {
				interruptionRecorded = false;
			}
		}
		if (interruptionRecorded && coreExec) {
			interruptionRecorded = await cleanupEnforcementRuntime(coreExec, {
				mode,
				action: "fs.read",
				class: "workspace_read",
				runId: authority.runId,
				nopalBin: authority.nopalBin,
				adapterCapability: authority.adapterCapability,
				launchId: authority.runId,
				sessionId: "session-shutdown",
				toolCallId: "session-shutdown",
				toolName: "session_shutdown",
				inputDigest: "session-shutdown",
				targetDigest: "session-shutdown",
				executorDigest: authority.gateExecutorDigest,
				runtimeDigest: authority.gateRuntimeDigest,
				cwd: authority.projectRoot,
			});
		}
		if (interruptionRecorded && coreExec) {
			interruptionRecorded = await closeEnforcementRun(
				coreExec,
				{ nopalBin: authority.nopalBin, runId: authority.runId, cwd: authority.projectRoot },
				runWasInterrupted ? "interrupted" : "completed",
				runWasInterrupted ? "Pi session shut down with unsettled protected calls" : "Pi session shut down",
			);
		} else {
			interruptionRecorded = false;
		}
		if (interruptionRecorded) {
			activeCalls.clear();
			activeMutatingCallId = undefined;
			concurrencyPoisoned = false;
			shuttingDown = false;
		} else {
			concurrencyPoisoned = true;
			stats.failClosed += 1;
			stats.blocked += 1;
		}
	});
}
