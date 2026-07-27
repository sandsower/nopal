/**
 * Subprocess helper for `nopal --json policy decide`.
 *
 * Pure module: the actual process spawn is injected as `exec` (matching the
 * shape of `pi.exec` from `@earendil-works/pi-coding-agent`), so tests can fake
 * it without spawning Pi or the real Nopal binary.
 *
 * Fail-closed contract: any problem reaching or parsing the nopal binary
 * (missing binary, nonzero exit, unparseable JSON, `ok: false`, or a
 * `decision` field that isn't one of allow/deny/ask) degrades to `ask`.
 * Callers block when there is no UI to ask through. Never silently allow.
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
};

/** Matches the call shape of `pi.exec` so `pi.exec` can be passed directly. */
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
		return failClosed(`nopal policy decide exited with code ${code}; treating as ask (fail closed)`);
	}

	let parsed: RawPolicyDecisionEnvelope;
	try {
		parsed = JSON.parse(stdout) as RawPolicyDecisionEnvelope;
	} catch {
		return failClosed("nopal policy decide produced unparseable output; treating as ask (fail closed)");
	}

	if (parsed.ok !== true) {
		const messages = extractDiagnosticMessages(parsed.diagnostics);
		return failClosed(
			messages.length > 0
				? `nopal policy decide reported an error: ${messages.join("; ")}`
				: "nopal policy decide reported ok: false; treating as ask (fail closed)",
		);
	}

	if (!isPolicyDecision(parsed.decision)) {
		return failClosed("nopal policy decide response is missing a recognized decision; treating as ask (fail closed)");
	}

	const explanation = extractExplanation(parsed.explanation) ?? `decision ${parsed.decision}`;
	return { decision: parsed.decision, explanation, failClosed: false };
}

export type GateRun = { command: string } | { argv: string[] };

export type EnforcementGate = {
	id: string;
	run: GateRun;
	cwd?: string;
};

export type EnforcementPlanResult = PolicyDecisionResult & {
	ok: boolean;
	root: string;
	requiredGates: EnforcementGate[];
};

export type EnforcementParams = {
	mode: string;
	action: string;
	class: string;
	runId: string;
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
		return failClosed(`nopal binary could not be executed (${message}); treating as ask (fail closed)`);
	}
	return parsePolicyDecisionOutput(result.stdout, result.code);
}

function validGateRun(value: unknown): value is GateRun {
	if (!value || typeof value !== "object") return false;
	const run = value as { command?: unknown; argv?: unknown };
	return typeof run.command === "string" || (Array.isArray(run.argv) && run.argv.every((arg) => typeof arg === "string"));
}

export function parseEnforcementPlanOutput(stdout: string, code: number): EnforcementPlanResult {
	const fallback = failClosed(`nopal enforcement plan exited with code ${code}; treating as ask (fail closed)`);
	if (code !== 0) return { ...fallback, ok: false, root: "", requiredGates: [] };
	try {
		const value = JSON.parse(stdout) as {
			kind?: unknown;
			ok?: unknown;
			root?: unknown;
			decision?: unknown;
			required_gates?: unknown;
		};
		if (value.kind !== "nopal.enforcement.plan/v1" || value.ok !== true || typeof value.root !== "string" || !isPolicyDecision(value.decision)) {
			throw new Error("invalid enforcement envelope");
		}
		if (!Array.isArray(value.required_gates)) throw new Error("invalid gate list");
		const requiredGates = value.required_gates.map((entry) => {
			if (!entry || typeof entry !== "object") throw new Error("invalid gate");
			const gate = entry as { id?: unknown; run?: unknown; cwd?: unknown };
			if (typeof gate.id !== "string" || !validGateRun(gate.run) || (gate.cwd !== undefined && typeof gate.cwd !== "string")) {
				throw new Error("invalid gate");
			}
			return { id: gate.id, run: gate.run, ...(gate.cwd === undefined ? {} : { cwd: gate.cwd }) };
		});
		return {
			ok: true,
			root: value.root,
			decision: value.decision,
			explanation: `effective decision ${value.decision}`,
			failClosed: false,
			requiredGates,
		};
	} catch {
		const invalid = failClosed("nopal enforcement plan produced an invalid envelope; treating as ask (fail closed)");
		return { ...invalid, ok: false, root: "", requiredGates: [] };
	}
}

export async function planEnforcement(exec: ExecFn, params: EnforcementParams): Promise<EnforcementPlanResult> {
	const args = [
		"--json", "enforcement", "plan", "--mode", params.mode, "--action", params.action,
		"--class", params.class, "--run-id", params.runId,
	];
	try {
		const result = await exec("nopal", args, { cwd: params.cwd, timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS });
		return parseEnforcementPlanOutput(result.stdout, result.code);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		const fallback = failClosed(`nopal binary could not be executed (${message}); treating as ask (fail closed)`);
		return { ...fallback, ok: false, root: "", requiredGates: [] };
	}
}

export async function recordEnforcementGate(
	exec: ExecFn,
	params: EnforcementParams & { gateId: string; exitCode: number },
): Promise<boolean> {
	const args = [
		"--json", "enforcement", "record-gate", "--mode", params.mode, "--action", params.action,
		"--class", params.class, "--run-id", params.runId, "--gate-id", params.gateId,
		"--exit-code", String(params.exitCode),
	];
	try {
		const result = await exec("nopal", args, { cwd: params.cwd, timeout: params.timeoutMs ?? DEFAULT_TIMEOUT_MS });
		if (result.code !== 0) return false;
		const value = JSON.parse(result.stdout) as { kind?: unknown; ok?: unknown };
		return value.kind === "nopal.enforcement.record_gate/v1" && value.ok === true;
	} catch {
		return false;
	}
}
