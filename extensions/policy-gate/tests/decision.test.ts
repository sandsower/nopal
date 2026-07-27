import assert from "node:assert/strict";
import { test } from "node:test";
import {
	decidePolicy,
	parseEnforcementPlanOutput,
	parsePolicyDecisionOutput,
	planEnforcement,
	reauthorizationIsCurrent,
	recordEnforcementGate,
	resolvePolicyMode,
	type ExecFn,
} from "../nopal-cli.ts";

function envelope(overrides: Record<string, unknown> = {}): string {
	return JSON.stringify({
		kind: "nopal.policy_decision/v1",
		ok: true,
		mode: "supervised_auto",
		action: "git.push",
		classes: ["git_remote"],
		decision: "allow",
		explanation: ["class git_remote: declared by caller", "decision allow: most restrictive decision from matched rules"],
		diagnostics: [],
		...overrides,
	});
}

test("resolvePolicyMode: defaults to supervised_auto", () => {
	assert.equal(resolvePolicyMode({}), "supervised_auto");
});

test("resolvePolicyMode: reads NOPAL_POLICY_MODE", () => {
	assert.equal(resolvePolicyMode({ NOPAL_POLICY_MODE: "manual" }), "manual");
});

test("resolvePolicyMode: blank env value falls back to default", () => {
	assert.equal(resolvePolicyMode({ NOPAL_POLICY_MODE: "   " }), "supervised_auto");
});

test("parsePolicyDecisionOutput: parses a real allow envelope", () => {
	const result = parsePolicyDecisionOutput(envelope(), 0);
	assert.equal(result.decision, "allow");
	assert.equal(result.failClosed, false);
	assert.match(result.explanation, /decision allow/);
});

test("parsePolicyDecisionOutput: parses a deny envelope", () => {
	const result = parsePolicyDecisionOutput(envelope({ decision: "deny" }), 0);
	assert.equal(result.decision, "deny");
	assert.equal(result.failClosed, false);
});

test("parsePolicyDecisionOutput: parses an ask envelope", () => {
	const result = parsePolicyDecisionOutput(envelope({ decision: "ask" }), 0);
	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, false);
});

test("parsePolicyDecisionOutput: nonzero exit code fails closed to ask", () => {
	const result = parsePolicyDecisionOutput("", 1);
	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
	assert.match(result.explanation, /exited with code 1/);
});

test("parsePolicyDecisionOutput: unparseable stdout fails closed to ask", () => {
	const result = parsePolicyDecisionOutput("not json at all", 0);
	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
	assert.match(result.explanation, /unparseable/);
});

test("parsePolicyDecisionOutput: ok:false with exit 0 still fails closed and surfaces diagnostic messages", () => {
	// A real Nopal exit code for ok:false is nonzero, but the parser must not
	// trust `decision` un-checked just because exit code is 0 - verified here
	// with exit 0 and no `decision` field at all (the actual ok:false shape).
	const output = JSON.stringify({
		ok: false,
		diagnostics: [{ severity: "error", code: "module_missing", message: "policy evaluation requires .nopal/policy.jsonc" }],
	});
	const result = parsePolicyDecisionOutput(output, 0);
	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
	assert.match(result.explanation, /module_missing|requires \.nopal\/policy\.jsonc/);
});

test("parsePolicyDecisionOutput: missing/unrecognized decision field fails closed to ask", () => {
	const output = JSON.stringify({ ok: true, decision: "maybe" });
	const result = parsePolicyDecisionOutput(output, 0);
	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
});

test("decidePolicy: happy path calls exec with the expected nopal invocation and parses the result", async () => {
	const calls: Array<{ command: string; args: string[]; options?: unknown }> = [];
	const exec: ExecFn = async (command, args, options) => {
		calls.push({ command, args, options });
		return { stdout: envelope({ decision: "allow" }), stderr: "", code: 0 };
	};

	const result = await decidePolicy(exec, { mode: "supervised_auto", action: "git.push", class: "git_remote", cwd: "/repo" });

	assert.equal(result.decision, "allow");
	assert.equal(calls.length, 1);
	assert.equal(calls[0].command, "nopal");
	assert.deepEqual(calls[0].args, ["--json", "policy", "decide", "--mode", "supervised_auto", "--action", "git.push", "--class", "git_remote"]);
	assert.deepEqual(calls[0].options, { cwd: "/repo", timeout: 10_000 });
});

test("decidePolicy: missing binary (exec throws) fails closed to ask, never silently allows", async () => {
	const exec: ExecFn = async () => {
		throw new Error("spawn nopal ENOENT");
	};

	const result = await decidePolicy(exec, { mode: "supervised_auto", action: "rm.recursive", class: "destructive" });

	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
	assert.match(result.explanation, /ENOENT|could not be executed/);
});

test("decidePolicy: nonzero exit from exec fails closed to ask", async () => {
	const exec: ExecFn = async () => ({ stdout: "", stderr: "nopal: command not found", code: 127 });

	const result = await decidePolicy(exec, { mode: "supervised_auto", action: "git.push", class: "git_remote" });

	assert.equal(result.decision, "ask");
	assert.equal(result.failClosed, true);
});

test("decidePolicy: deny decision is passed through unchanged", async () => {
	const exec: ExecFn = async () => ({ stdout: envelope({ decision: "deny", action: "rm.recursive", classes: ["destructive"] }), stderr: "", code: 0 });

	const result = await decidePolicy(exec, { mode: "supervised_auto", action: "rm.recursive", class: "destructive" });

	assert.equal(result.decision, "deny");
	assert.equal(result.failClosed, false);
});

test("parseEnforcementPlanOutput accepts selected command and argv gates", () => {
	const result = parseEnforcementPlanOutput(JSON.stringify({
		kind: "nopal.enforcement.plan/v1",
		ok: true,
		root: "/repo",
		decision: "allow",
		required_gates: [
			{ id: "fmt", run: { command: "cargo fmt --check" } },
			{ id: "test", run: { argv: ["cargo", "test"] }, cwd: "crate" },
		],
		receipts: [
			{ gate_id: "fmt", current: false, gate_definition_digest: "fmt-digest" },
			{ gate_id: "test", current: false, gate_definition_digest: "test-digest" },
		],
		contract_digest: "contract",
		workspace_fingerprint: "workspace",
	}), 0);
	assert.equal(result.failClosed, false);
	assert.equal(result.root, "/repo");
	assert.deepEqual(result.requiredGates.map((gate) => gate.id), ["fmt", "test"]);
});

test("enforcement subprocess helpers use the initialized run and fail closed", async () => {
	const calls: Array<{ command: string; args: string[] }> = [];
	const exec: ExecFn = async (command, args) => {
		calls.push({ command, args });
		if (args.includes("record-gate")) {
			return { stdout: JSON.stringify({ kind: "nopal.enforcement.record_gate/v1", ok: true }), stderr: "", code: 0 };
		}
		return {
			stdout: JSON.stringify({
				kind: "nopal.enforcement.plan/v1",
				ok: true,
				root: "/repo",
				decision: "deny",
				required_gates: [],
				receipts: [],
				contract_digest: "contract",
				workspace_fingerprint: "workspace",
			}),
			stderr: "",
			code: 0,
		};
	};
	const params = {
		mode: "supervised_auto",
		action: "git.push_force",
		class: "git_remote",
		runId: "run-1",
		nopalBin: "/distribution/bin/nopal",
	};
	assert.equal((await planEnforcement(exec, params)).decision, "deny");
	assert.equal(await recordEnforcementGate(exec, {
		...params,
		gateId: "fmt",
		exitCode: 0,
		contractDigest: "contract",
		workspaceFingerprint: "workspace",
		gateDefinitionDigest: "fmt-digest",
	}), true);
	assert.ok(calls.every((call) => call.args.includes("run-1")));
	assert.ok(calls.every((call) => call.command === params.nopalBin));
	assert.ok(calls.every((call) => !call.args.some((argument) => argument.includes("0123456789abcdef"))));
});

test("reauthorization preserves an approved ask only for the exact current context", () => {
	const initial = parseEnforcementPlanOutput(JSON.stringify({
		kind: "nopal.enforcement.plan/v1",
		ok: true,
		root: "/repo",
		decision: "ask",
		required_gates: [{ id: "proof", run: { command: "true" } }],
		receipts: [{ gate_id: "proof", current: false, gate_definition_digest: "proof-digest" }],
		contract_digest: "contract",
		workspace_fingerprint: "workspace",
	}), 0);
	const current = parseEnforcementPlanOutput(JSON.stringify({
		kind: "nopal.enforcement.plan/v1",
		ok: true,
		root: "/repo",
		decision: "ask",
		required_gates: [],
		receipts: [{ gate_id: "proof", current: true, gate_definition_digest: "proof-digest" }],
		contract_digest: "contract",
		workspace_fingerprint: "workspace",
	}), 0);
	assert.equal(reauthorizationIsCurrent(initial, current, true), true);
	assert.equal(reauthorizationIsCurrent(initial, current, false), false);
	assert.equal(reauthorizationIsCurrent(initial, { ...current, contractDigest: "changed" }, true), false);
	assert.equal(
		reauthorizationIsCurrent(initial, { ...current, decision: "allow", workspaceFingerprint: "changed" }, true),
		false,
	);
});
