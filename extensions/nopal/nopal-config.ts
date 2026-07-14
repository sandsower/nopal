/**
 * Config reads for the Nopal session extension.
 *
 * Replaces beislid's `config.ts` + `babysit-config.ts`: there is no jsonc
 * parsing, fenced-block parsing, or `~/.pi/agent/beislid.json` /
 * `.pi/beislid.json` override chain here anymore. The single source of
 * truth is one subprocess call, `nopal --json workflow show`, cached per
 * session and refreshed on `session_start`. Defaults are applied by the
 * core; this module only supplies a safe fallback for when the CLI itself
 * cannot be consulted (missing binary, nonzero exit, unparseable output).
 */

import { fetchWorkflowShow, type ExecFn, type WorkflowShowResult } from "./nopal-cli.js";

export type HandoffConfig = {
	autoHandoff: boolean;
	events: Set<string> | "all";
	exclude: Set<string>;
};

export type NopalWorkflowConfig = {
	available: boolean;
	handoff: HandoffConfig;
	babysitTokenBudget: number | null;
	establishmentEvents: Set<string>;
};

/**
 * Fallback used only when `nopal workflow show` could not be consulted at
 * all. This is deliberately more conservative than the core's own
 * defaults (auto-handoff off, no budget) rather than re-implementing the
 * core's default policy in TypeScript.
 */
export const FALLBACK_CONFIG: NopalWorkflowConfig = {
	available: false,
	handoff: { autoHandoff: false, events: "all", exclude: new Set() },
	babysitTokenBudget: null,
	establishmentEvents: new Set(),
};

export function toWorkflowConfig(result: WorkflowShowResult | undefined): NopalWorkflowConfig {
	if (!result) return FALLBACK_CONFIG;
	const events = result.handoff.events.length === 0 ? ("all" as const) : new Set(result.handoff.events);
	return {
		available: true,
		handoff: {
			autoHandoff: result.handoff.auto,
			events,
			exclude: new Set(result.handoff.exclude),
		},
		babysitTokenBudget: result.babysit.tokenBudget,
		establishmentEvents: new Set(result.establishment?.events ?? []),
	};
}

/** Run `nopal --json workflow show` and normalize it, falling back to safe defaults on failure. */
export async function resolveNopalWorkflowConfig(exec: ExecFn, cwd: string): Promise<NopalWorkflowConfig> {
	const result = await fetchWorkflowShow(exec, cwd);
	return toWorkflowConfig(result);
}

export type NopalConfigCache = {
	/** Get the cached config, fetching once and memoizing until `refresh()`. */
	get(cwd: string): Promise<NopalWorkflowConfig>;
	/** Drop the cached value; the next `get()` re-fetches. Call on `session_start`. */
	refresh(): void;
};

/** Per-session memoizing cache around `resolveNopalWorkflowConfig`. */
export function createNopalConfigCache(exec: ExecFn): NopalConfigCache {
	let cached: Promise<NopalWorkflowConfig> | undefined;
	return {
		get(cwd: string): Promise<NopalWorkflowConfig> {
			if (!cached) cached = resolveNopalWorkflowConfig(exec, cwd);
			return cached;
		},
		refresh(): void {
			cached = undefined;
		},
	};
}

// ---------------------------------------------------------------------------
// Babysit token budget argument parsing (ported from beislid's babysit-config.ts)
// ---------------------------------------------------------------------------

type TokenArg = {
	args: string;
	tokenBudget?: string;
};

/** Split a leading/trailing `--tokens <n>` (or `--tokens=<n>`) flag out of babysit invocation args. */
export function splitBabysitTokenBudgetArg(args: string): TokenArg {
	const trimmed = args.trim();
	const tokenFlagPattern = /(?:^|\s)--tokens(?:(?:=|\s+)(\S+))?(?=\s|$)/g;
	const validBudgetPattern = /^([0-9]+(?:\.[0-9]+)?)([kKmM]?)$/;
	let tokenBudget: string | undefined;
	let withoutToken = "";
	let lastIndex = 0;
	for (const match of trimmed.matchAll(tokenFlagPattern)) {
		const candidate = match[1] ?? "";
		const validBudget = candidate.match(validBudgetPattern);
		if (tokenBudget === undefined && validBudget && Number(validBudget[1]) > 0) {
			tokenBudget = candidate;
		}
		withoutToken += `${trimmed.slice(lastIndex, match.index)} `;
		lastIndex = match.index + match[0].length;
	}
	withoutToken += trimmed.slice(lastIndex);
	return { args: withoutToken.replace(/\s+/g, " ").trim(), tokenBudget };
}

/** Parse a validated `--tokens` value ("400000", "400k", "1.5m", ...) into a token count. */
export function parseTokenBudgetArg(raw: string | undefined): number | null {
	if (!raw) return null;
	const match = raw.match(/^([0-9]+(?:\.[0-9]+)?)([kKmM]?)$/);
	if (!match) return null;
	const value = Number(match[1]);
	if (!Number.isFinite(value) || value <= 0) return null;
	const suffix = match[2].toLowerCase();
	const multiplier = suffix === "m" ? 1_000_000 : suffix === "k" ? 1_000 : 1;
	return Math.round(value * multiplier);
}
