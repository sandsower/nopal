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
