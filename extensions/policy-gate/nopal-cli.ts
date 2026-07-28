/**
 * Subprocess helper for `nopal --json policy decide`.
 *
 * Pure module: the actual process spawn is injected as `exec` (matching the
 * shape of `pi.exec` from `@earendil-works/pi-coding-agent`), so tests can fake
 * it without spawning Pi or the real Nopal binary.
 *
 * Fail-closed contract: any transport, process, schema, or identity failure
 * sets `failClosed`. The compatibility decision field remains `ask`, but the
 * guard must block that result before UI and may never approve around it.
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
	input?: string;
};

/** The trusted adapter may use a private stdin-capable process implementation. */
export type ExecFn = (command: string, args: string[], options?: ExecOptions) => Promise<ExecResult>;

export type PolicyDecision = "allow" | "deny" | "ask";

export type PolicyDecisionResult = {
	decision: PolicyDecision;
	explanation: string;
	/** True when this result is a fallback because the CLI could not be consulted, not a real core verdict. */
	failClosed: boolean;
};

export const DEFAULT_POLICY_MODE = "supervised_auto";
const POLICY_MODE_ENV_VAR = "NOPAL_POLICY_MODE";
const DEFAULT_TIMEOUT_MS = 10_000;
const ADAPTER_PROOF_KIND = "nopal.enforcement.adapter_proof/v1";

function adapterProof(capability?: string): string {
	return `${JSON.stringify({ kind: ADAPTER_PROOF_KIND, capability: capability ?? "" })}\n`;
}

/** Resolve the policy mode from `NOPAL_POLICY_MODE`, defaulting to `supervised_auto`. */
export function resolvePolicyMode(env: NodeJS.ProcessEnv = process.env): string {
	const raw = env[POLICY_MODE_ENV_VAR];
	return typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : DEFAULT_POLICY_MODE;
}

function failClosed(reason: string): PolicyDecisionResult {
	return { decision: "ask", explanation: reason, failClosed: true };
}

function isPolicyDecision(value: unknown): value is PolicyDecision {
	return value === "allow" || value === "deny" || value === "ask";
}

function extractExplanation(value: unknown): string | undefined {
	if (!Array.isArray(value)) return undefined;
	const lines = value.filter((line): line is string => typeof line === "string");
	return lines.length > 0 ? lines.join("; ") : undefined;
}

function extractDiagnosticMessages(value: unknown): string[] {
	if (!Array.isArray(value)) return [];
	return value
		.map((entry) => (entry && typeof entry === "object" ? (entry as { message?: unknown }).message : undefined))
		.filter((message): message is string => typeof message === "string");
}

type RawPolicyDecisionEnvelope = {
	ok?: unknown;
	decision?: unknown;
	explanation?: unknown;
	diagnostics?: unknown;
};

/**
 * Parse the stdout/exit code of a `nopal --json policy decide` invocation
 * into a normalized decision. Exported separately from `decidePolicy` so
 * tests can feed it captured output without an exec seam at all.
 */
export function parsePolicyDecisionOutput(stdout: string, code: number): PolicyDecisionResult {
	if (code !== 0) {
		return failClosed(`nopal policy decide exited with code ${code}; authorization unavailable (fail closed)`);
	}

	let parsed: RawPolicyDecisionEnvelope;
	try {
		parsed = JSON.parse(stdout) as RawPolicyDecisionEnvelope;
	} catch {
		return failClosed("nopal policy decide produced unparseable output; authorization unavailable (fail closed)");
	}

	if (parsed.ok !== true) {
		const messages = extractDiagnosticMessages(parsed.diagnostics);
		return failClosed(
			messages.length > 0
				? `nopal policy decide reported an error: ${messages.join("; ")}`
				: "nopal policy decide reported ok: false; authorization unavailable (fail closed)",
		);
	}

	if (!isPolicyDecision(parsed.decision)) {
		return failClosed("nopal policy decide response is missing a recognized decision; authorization unavailable (fail closed)");
	}

	const explanation = extractExplanation(parsed.explanation) ?? `decision ${parsed.decision}`;
	return { decision: parsed.decision, explanation, failClosed: false };
}

export type GateRun = { command: string } | { argv: string[] };

export type EnforcementGate = {
	id: string;
	run: GateRun;
	cwd?: string;
	autofix?: string;
	parallelSafe?: boolean;
	mutates?: boolean;
	definitionDigest: string;
};

export type EnforcementPlanResult = PolicyDecisionResult & {
	ok: boolean;
	root: string;
	placement: string;
	decisionWinners: string[];
	placementWinners: string[];
	requiredStages: string[];
	requiredGates: EnforcementGate[];
	contractDigest: string;
	workspaceFingerprint: string;
	authorizationBinding: string;
	approvalCurrent: boolean;
	authorized: boolean;
};

export type EnforcementParams = {
	mode: string;
	action: string;
	class: string;
	runId: string;
	nopalBin: string;
	adapterCapability?: string;
	launchId?: string;
	sessionId?: string;
	toolCallId?: string;
	toolName?: string;
	inputDigest?: string;
	targetDigest?: string;
	executorDigest?: string;
	changedFiles?: string[];
	mutates?: boolean;
	cwd?: string;
	timeoutMs?: number;
};

export type DecidePolicyParams = {
	mode: string;
	action: string;
	class: string;
	cwd?: string;
	timeoutMs?: number;
};

/**
 * Run `nopal --json policy decide --mode <mode> --action <action> --class <class>`
 * via the injected `exec` and return the normalized decision. Never throws:
 * exec failures (missing binary, spawn errors) fail closed to `ask`.
 */
export async function decidePolicy(exec: ExecFn, params: DecidePolicyParams): Promise<PolicyDecisionResult> {
	const args = ["--json", "policy", "decide", "--mode", params.mode, "--action", params.action, "--class", params.class];
	let result: ExecResult;
	try {
		result = await exec("nopal", args, { cwd: params.cwd, timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS });
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return failClosed(`nopal binary could not be executed (${message}); authorization unavailable (fail closed)`);
	}
	return parsePolicyDecisionOutput(result.stdout, result.code);
}

function validGateRun(value: unknown): value is GateRun {
	if (!value || typeof value !== "object") return false;
	const run = value as { command?: unknown; argv?: unknown };
	return typeof run.command === "string" || (Array.isArray(run.argv) && run.argv.every((arg) => typeof arg === "string"));
}

export function parseEnforcementPlanOutput(stdout: string, code: number): EnforcementPlanResult {
	const fallback = failClosed(`nopal enforcement plan exited with code ${code}; authorization unavailable (fail closed)`);
	const empty = {
		ok: false,
		root: "",
		placement: "blocked",
		decisionWinners: [],
		placementWinners: [],
		requiredStages: [],
		requiredGates: [],
		contractDigest: "",
		workspaceFingerprint: "",
		authorizationBinding: "",
		approvalCurrent: false,
		authorized: false,
	};
	if (code !== 0) return { ...fallback, ...empty };
	try {
		const value = JSON.parse(stdout) as {
			kind?: unknown;
			ok?: unknown;
			root?: unknown;
			decision?: unknown;
			required_gates?: unknown;
			receipts?: unknown;
			contract_digest?: unknown;
			workspace_fingerprint?: unknown;
			placement?: unknown;
			decision_winners?: unknown;
			placement_winners?: unknown;
			required_stages?: unknown;
			authorization_binding?: unknown;
			approval_current?: unknown;
			authorized?: unknown;
		};
		if (
			value.kind !== "nopal.enforcement.plan/v2"
			|| value.ok !== true
			|| typeof value.root !== "string"
			|| !isPolicyDecision(value.decision)
			|| typeof value.contract_digest !== "string"
			|| typeof value.workspace_fingerprint !== "string"
			|| typeof value.placement !== "string"
			|| !Array.isArray(value.decision_winners)
			|| !value.decision_winners.every((entry) => typeof entry === "string")
			|| !Array.isArray(value.placement_winners)
			|| !value.placement_winners.every((entry) => typeof entry === "string")
			|| !Array.isArray(value.required_stages)
			|| !value.required_stages.every((entry) => typeof entry === "string")
			|| typeof value.authorization_binding !== "string"
			|| typeof value.approval_current !== "boolean"
			|| typeof value.authorized !== "boolean"
		) throw new Error("invalid enforcement envelope");
		if (!Array.isArray(value.required_gates) || !Array.isArray(value.receipts)) throw new Error("invalid gate evidence");
		const definitions = new Map<string, string>();
		for (const entry of value.receipts) {
			if (!entry || typeof entry !== "object") throw new Error("invalid receipt status");
			const receipt = entry as { gate_id?: unknown; gate_definition_digest?: unknown };
			if (typeof receipt.gate_id !== "string" || typeof receipt.gate_definition_digest !== "string") {
				throw new Error("invalid receipt status");
			}
			definitions.set(receipt.gate_id, receipt.gate_definition_digest);
		}
		const requiredGates = value.required_gates.map((entry) => {
			if (!entry || typeof entry !== "object") throw new Error("invalid gate");
			const gate = entry as {
				id?: unknown;
				run?: unknown;
				cwd?: unknown;
				autofix?: unknown;
				parallel_safe?: unknown;
				mutates?: unknown;
			};
			if (
				typeof gate.id !== "string"
				|| !validGateRun(gate.run)
				|| (gate.cwd !== undefined && gate.cwd !== null && typeof gate.cwd !== "string")
				|| (gate.autofix !== undefined && gate.autofix !== null && typeof gate.autofix !== "string")
				|| (gate.parallel_safe !== undefined && gate.parallel_safe !== null && typeof gate.parallel_safe !== "boolean")
				|| (gate.mutates !== undefined && gate.mutates !== null && typeof gate.mutates !== "boolean")
			) {
				throw new Error("invalid gate");
			}
			const definitionDigest = definitions.get(gate.id);
			if (!definitionDigest) throw new Error("missing gate definition digest");
			return {
				id: gate.id,
				run: gate.run,
				definitionDigest,
				...(typeof gate.cwd === "string" ? { cwd: gate.cwd } : {}),
				...(typeof gate.autofix === "string" ? { autofix: gate.autofix } : {}),
				...(typeof gate.parallel_safe === "boolean" ? { parallelSafe: gate.parallel_safe } : {}),
				...(typeof gate.mutates === "boolean" ? { mutates: gate.mutates } : {}),
			};
		});
		const decisionWinners = value.decision_winners as string[];
		const placementWinners = value.placement_winners as string[];
		return {
			ok: true,
			root: value.root,
			decision: value.decision,
			explanation: `effective decision ${value.decision} from ${decisionWinners.join(", ") || "built-in floor"}; placement ${value.placement} from ${placementWinners.join(", ") || "built-in floor"}`,
			failClosed: false,
			placement: value.placement,
			decisionWinners,
			placementWinners,
			requiredStages: value.required_stages as string[],
			requiredGates,
			contractDigest: value.contract_digest,
			workspaceFingerprint: value.workspace_fingerprint,
			authorizationBinding: value.authorization_binding,
			approvalCurrent: value.approval_current,
			authorized: value.authorized,
		};
	} catch {
		const invalid = failClosed("nopal enforcement plan produced an invalid envelope; authorization unavailable (fail closed)");
		return { ...invalid, ...empty };
	}
}

export function reauthorizationIsCurrent(
	initial: EnforcementPlanResult,
	reauthorized: EnforcementPlanResult,
	approvedAsk: boolean,
): boolean {
	const contextUnchanged = reauthorized.contractDigest === initial.contractDigest
		&& reauthorized.workspaceFingerprint === initial.workspaceFingerprint
		&& reauthorized.authorizationBinding === initial.authorizationBinding;
	const decisionAuthorized = reauthorized.decision === "allow"
		|| (reauthorized.decision === "ask" && approvedAsk);
	return contextUnchanged
		&& !reauthorized.failClosed
		&& decisionAuthorized
		&& reauthorized.requiredGates.length === 0;
}

export async function planEnforcement(exec: ExecFn, params: EnforcementParams): Promise<EnforcementPlanResult> {
	const args = [
		"--json", "enforcement", "plan", "--mode", params.mode, "--action", params.action,
		"--class", params.class, "--run-id", params.runId,
		"--launch-id", params.launchId ?? "legacy",
		"--session-id", params.sessionId ?? "legacy",
		"--tool-call-id", params.toolCallId ?? "legacy",
		"--tool-name", params.toolName ?? "legacy",
		"--input-digest", params.inputDigest ?? "legacy",
		"--target-digest", params.targetDigest ?? "legacy",
		"--executor-digest", params.executorDigest ?? "legacy-executor",
		...(params.changedFiles ?? []).flatMap((file) => ["--changed-file", file]),
		...(params.mutates ? ["--mutates"] : []),
	];
	try {
		const result = await exec(params.nopalBin, args, {
			cwd: params.cwd,
			timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS,
			input: adapterProof(params.adapterCapability),
		});
		return parseEnforcementPlanOutput(result.stdout, result.code);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		const fallback = failClosed(`nopal binary could not be executed (${message}); authorization unavailable (fail closed)`);
		return {
			...fallback,
			ok: false,
			root: "",
			placement: "blocked",
			decisionWinners: [],
			placementWinners: [],
			requiredStages: [],
			requiredGates: [],
			contractDigest: "",
			workspaceFingerprint: "",
			authorizationBinding: "",
			approvalCurrent: false,
			authorized: false,
		};
	}
}

export async function recordEnforcementGate(
	exec: ExecFn,
	params: EnforcementParams & {
		gateId: string;
		exitCode: number;
		contractDigest: string;
		workspaceFingerprint: string;
		gateDefinitionDigest: string;
		authorizationBinding: string;
	},
): Promise<boolean> {
	const args = [
		"--json", "enforcement", "record-gate", "--mode", params.mode, "--action", params.action,
		"--class", params.class, "--run-id", params.runId,
		"--launch-id", params.launchId ?? "legacy",
		"--session-id", params.sessionId ?? "legacy",
		"--tool-call-id", params.toolCallId ?? "legacy",
		"--tool-name", params.toolName ?? "legacy",
		"--input-digest", params.inputDigest ?? "legacy",
		"--target-digest", params.targetDigest ?? "legacy",
		"--executor-digest", params.executorDigest ?? "legacy-executor",
		...(params.changedFiles ?? []).flatMap((file) => ["--changed-file", file]),
		...(params.mutates ? ["--mutates"] : []),
		"--gate-id", params.gateId, "--exit-code", String(params.exitCode),
		"--contract-digest", params.contractDigest,
		"--workspace-fingerprint", params.workspaceFingerprint,
		"--gate-definition-digest", params.gateDefinitionDigest,
		"--authorization-binding", params.authorizationBinding,
	];
	try {
		const result = await exec(params.nopalBin, args, {
			cwd: params.cwd,
			timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS,
			input: adapterProof(params.adapterCapability),
		});
		if (result.code !== 0) return false;
		const value = JSON.parse(result.stdout) as { kind?: unknown; ok?: unknown };
		return value.kind === "nopal.enforcement.record_gate/v2" && value.ok === true;
	} catch {
		return false;
	}
}

function exactIntentArgs(operation: string, params: EnforcementParams): string[] {
	return [
		"--json", "enforcement", operation, "--mode", params.mode, "--action", params.action,
		"--class", params.class, "--run-id", params.runId,
		"--launch-id", params.launchId ?? "legacy",
		"--session-id", params.sessionId ?? "legacy",
		"--tool-call-id", params.toolCallId ?? "legacy",
		"--tool-name", params.toolName ?? "legacy",
		"--input-digest", params.inputDigest ?? "legacy",
		"--target-digest", params.targetDigest ?? "legacy",
		"--executor-digest", params.executorDigest ?? "legacy-executor",
		...(params.changedFiles ?? []).flatMap((file) => ["--changed-file", file]),
		...(params.mutates ? ["--mutates"] : []),
	];
}

export async function recordEnforcementApproval(
	exec: ExecFn,
	params: EnforcementParams & { authorizationBinding: string; approved: boolean },
): Promise<boolean> {
	const args = [
		...exactIntentArgs("record-approval", params),
		"--authorization-binding", params.authorizationBinding,
		...(params.approved ? ["--approved"] : []),
	];
	try {
		const result = await exec(params.nopalBin, args, {
			cwd: params.cwd,
			timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS,
			input: adapterProof(params.adapterCapability),
		});
		if (result.code !== 0) return false;
		const value = JSON.parse(result.stdout) as { kind?: unknown; ok?: unknown };
		return value.kind === "nopal.enforcement.record_approval/v1" && value.ok === true;
	} catch {
		return false;
	}
}

export async function authorizeEnforcement(
	exec: ExecFn,
	params: EnforcementParams & { authorizationBinding: string },
): Promise<string | undefined> {
	const args = [
		...exactIntentArgs("authorize", params),
		"--authorization-binding", params.authorizationBinding,
	];
	try {
		const result = await exec(params.nopalBin, args, {
			cwd: params.cwd,
			timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS,
			input: adapterProof(params.adapterCapability),
		});
		if (result.code !== 0) return undefined;
		const value = JSON.parse(result.stdout) as { kind?: unknown; ok?: unknown; release_id?: unknown };
		return value.kind === "nopal.enforcement.authorization/v1"
			&& value.ok === true
			&& typeof value.release_id === "string"
			&& value.release_id.length > 0
			? value.release_id
			: undefined;
	} catch {
		return undefined;
	}
}

export async function recordEnforcementOutcome(
	exec: ExecFn,
	params: EnforcementParams & {
		authorizationBinding: string;
		releaseId: string;
		outcome: "success" | "error" | "cancelled" | "interrupted";
	},
): Promise<boolean> {
	const args = [
		...exactIntentArgs("record-outcome", params),
		"--authorization-binding", params.authorizationBinding,
		"--release-id", params.releaseId,
		"--outcome", params.outcome,
	];
	try {
		const result = await exec(params.nopalBin, args, {
			cwd: params.cwd,
			timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS,
			input: adapterProof(params.adapterCapability),
		});
		if (result.code !== 0) return false;
		const value = JSON.parse(result.stdout) as { kind?: unknown; ok?: unknown };
		return value.kind === "nopal.enforcement.record_outcome/v1" && value.ok === true;
	} catch {
		return false;
	}
}
