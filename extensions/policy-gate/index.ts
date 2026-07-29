import { closeSync, readSync, writeFileSync } from "node:fs";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { EnforcementAuthority } from "./classifier.js";
import {
	activeToolCatalogIsExpected,
	createGuardStats,
	installPiActionGuard,
	type GuardStats,
} from "./guard.js";
import { resolvePolicyMode } from "./nopal-cli.js";

type EnforcementBootstrap = {
	authority: EnforcementAuthority;
	mode: string;
};

const BOOTSTRAP_PROPERTY = "__nopalEnforcementBootstrapV2";

/**
 * Consume launch authority exactly once and retain it in a non-writable slot.
 * Agent tools and gate subprocesses inherit neither the bootstrap variable
 * names nor the launch-scoped mode after extension initialization.
 */
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
	const capabilityFdText = process.env.NOPAL_ENFORCEMENT_CAPABILITY_FD;
	const gateExecutorBin = process.env.NOPAL_GATE_EXECUTOR_BIN;
	const gateHome = process.env.NOPAL_GATE_HOME;
	const gateExecutorDigest = process.env.NOPAL_GATE_EXECUTOR_DIGEST;
	const gateRuntimeDigest = process.env.NOPAL_GATE_RUNTIME_DIGEST;
	const capabilityFd = Number(capabilityFdText);
	let adapterCapability: string | undefined;
	if (Number.isSafeInteger(capabilityFd) && capabilityFd >= 3 && capabilityFd <= 1024) {
		const bytes = Buffer.alloc(64);
		try {
			if (readSync(capabilityFd, bytes, 0, bytes.length, 0) === bytes.length) {
				const value = bytes.toString("utf8");
				if (/^[0-9a-f]{64}$/i.test(value)) adapterCapability = value;
			}
		} finally {
			closeSync(capabilityFd);
		}
	}
	const mode = resolvePolicyMode(process.env);
	delete process.env.NOPAL_ENFORCEMENT_RUN_ID;
	delete process.env.NOPAL_ENFORCEMENT_ROOT;
	delete process.env.NOPAL_ENFORCEMENT_STATE_DIR;
	delete process.env.NOPAL_ENFORCEMENT_CONFIG_DIR;
	delete process.env.NOPAL_ENFORCEMENT_ADAPTER_DIR;
	delete process.env.NOPAL_ENFORCEMENT_CLI;
	delete process.env.NOPAL_ENFORCEMENT_CAPABILITY_FD;
	delete process.env.NOPAL_GATE_EXECUTOR_BIN;
	delete process.env.NOPAL_GATE_HOME;
	delete process.env.NOPAL_GATE_EXECUTOR_DIGEST;
	delete process.env.NOPAL_GATE_RUNTIME_DIGEST;
	delete process.env.NOPAL_POLICY_MODE;
	if (!runId || !projectRoot || !stateDir || !adapterDir || !nopalBin || !adapterCapability
		|| !gateExecutorBin || !gateHome || !gateExecutorDigest || !gateRuntimeDigest
		|| !Number.isSafeInteger(capabilityFd)) return undefined;

	const bootstrap: EnforcementBootstrap = {
		authority: {
			runId,
			projectRoot,
			stateDir,
			adapterDir,
			nopalBin,
			adapterCapability,
			gateExecutorBin,
			gateHome,
			gateExecutorDigest,
			gateRuntimeDigest,
			...(configDir ? { configDir } : {}),
		},
		mode,
	};
	Object.defineProperty(host, BOOTSTRAP_PROPERTY, {
		value: bootstrap,
		enumerable: false,
		writable: false,
		configurable: false,
	});
	return bootstrap;
}

function notify(ctx: ExtensionContext, message: string, level: "info" | "warning" | "error" = "info"): void {
	if (ctx.hasUI) ctx.ui.notify(message, level);
}

function buildStatusLines(mode: string, stats: GuardStats): string[] {
	return [
		"Nopal enforcement: ON for the entire session",
		`Pinned policy mode: ${mode}`,
		"Protected surface: every active Pi tool call, exact targets, workflow gates, placement, credential paths, and enforcement authority",
		`Decisions this session: ${stats.total} (allowed ${stats.allowed}, denied ${stats.denied}, asked ${stats.asked}, approved ${stats.approved}, blocked ${stats.blocked}, fail-closed ${stats.failClosed})`,
	];
}

export default function policyGate(pi: ExtensionAPI) {
	let guardInstalled = false;
	const probe = process.env.NOPAL_ENFORCEMENT_PROBE === "1";
	pi.on("session_start", () => {
		const catalogExpected = activeToolCatalogIsExpected(pi.getActiveTools());
		if (!catalogExpected) process.exit(72);
		if (!probe) return;
		const path = process.env.NOPAL_ENFORCEMENT_PROBE_ACK;
		const token = process.env.NOPAL_ENFORCEMENT_PROBE_TOKEN;
		delete process.env.NOPAL_ENFORCEMENT_PROBE;
		delete process.env.NOPAL_ENFORCEMENT_PROBE_ACK;
		delete process.env.NOPAL_ENFORCEMENT_PROBE_TOKEN;
		if (!path || !token || !guardInstalled) process.exit(72);
		writeFileSync(path, token, { encoding: "utf8", mode: 0o600, flag: "wx" });
		process.exit(0);
	});
	const stats = createGuardStats();
	const bootstrap = loadBootstrap();
	const mode = bootstrap?.mode ?? "uninitialized";

	pi.registerCommand("policy-gate", {
		description: "Show the always-on Nopal enforcement status",
		handler: async (args, ctx) => {
			const token = args.trim().toLowerCase();
			if (token && token !== "status") {
				notify(ctx, "Usage: /policy-gate [status]", "warning");
				return;
			}
			notify(ctx, buildStatusLines(mode, stats).join("\n"), bootstrap ? "info" : "error");
		},
	});

	installPiActionGuard(pi, bootstrap?.authority, mode, stats);
	guardInstalled = true;
}
