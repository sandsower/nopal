import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { createGuardStats, installPiActionGuard } from "../guard.ts";

function envelope(root: string, decision = "allow", approved = false, requiredGates: unknown[] = []) {
	return JSON.stringify({
		kind: "nopal.enforcement.plan/v2",
		ok: true,
		root,
		action: "fs.write",
		decision,
		decision_winners: ["repository policy"],
		placement: "shared_user_runtime",
		placement_winners: ["repository policy"],
		decisions: [],
		required_stages: ["continuous", "per_edit"],
		required_gates: requiredGates,
		receipts: requiredGates.map((gate: any) => ({
			gate_id: gate.id,
			current: false,
			gate_definition_digest: `digest-${gate.id}`,
		})),
		contract_digest: "contract",
		workspace_fingerprint: "workspace",
		authorization_binding: "binding",
		approval_current: approved,
		authorized: decision === "allow" || approved,
		intent: {},
		diagnostics: [],
	});
}

function harness(
	root: string,
	decision = "allow",
	hasUI = false,
	gateCommand?: string,
	outcomeFails = false,
	gateExecutorBin = "/trusted/gate-bin",
	gateMetadata: Record<string, unknown> = {},
) {
	const handlers = new Map<string, Array<(event: any, ctx: any) => Promise<any>>>();
	const calls: Array<{ command: string; args: string[] }> = [];
	const outcomes: string[] = [];
	let approvalRecorded = false;
	const gateRecorded = new Set<string>();
	const pi = {
		on(name: string, handler: (event: any, ctx: any) => Promise<any>) {
			handlers.set(name, [...(handlers.get(name) ?? []), handler]);
		},
		async exec(command: string, args: string[]) {
			calls.push({ command, args });
			if (args.includes("authorize")) {
				return {
					stdout: JSON.stringify({ kind: "nopal.enforcement.authorization/v1", ok: true, release_id: "release-1" }),
					stderr: "",
					code: 0,
				};
			}
			if (args.includes("record-outcome")) {
				const outcome = args[args.indexOf("--outcome") + 1] ?? "missing";
				outcomes.push(outcome);
				return {
					stdout: JSON.stringify({ kind: "nopal.enforcement.record_outcome/v1", ok: !outcomeFails }),
					stderr: "",
					code: outcomeFails ? 2 : 0,
				};
			}
			if (args.includes("record-gate")) {
				gateRecorded.add(args[args.indexOf("--tool-call-id") + 1] ?? "missing");
				return { stdout: JSON.stringify({ kind: "nopal.enforcement.record_gate/v2", ok: true }), stderr: "", code: 0 };
			}
			if (args.includes("record-approval")) {
				approvalRecorded = args.includes("--approved");
				return { stdout: JSON.stringify({ kind: "nopal.enforcement.record_approval/v1", ok: true }), stderr: "", code: 0 };
			}
			const toolCallId = args[args.indexOf("--tool-call-id") + 1] ?? "missing";
			const requiredGates = gateCommand && !gateRecorded.has(toolCallId)
				? [{ id: "proof", run: { command: gateCommand }, ...gateMetadata }]
				: [];
			return { stdout: envelope(root, decision, approvalRecorded, requiredGates), stderr: "", code: 0 };
		},
	};
	const ctx = {
		cwd: root,
		hasUI,
		ui: { select: async () => "Yes, run it" },
		sessionManager: { getSessionFile: () => "/sessions/proof.jsonl" },
	};
	const stats = createGuardStats();
	installPiActionGuard(pi as any, {
		projectRoot: root,
		stateDir: path.join(root, ".state"),
		adapterDir: path.join(root, ".adapter"),
		nopalBin: "/distribution/nopal",
		runId: "run-1",
		adapterCapability: "capability",
		gateExecutorBin,
		gateHome: path.join(root, ".gate-home"),
		gateExecutorDigest: "executor-test",
	}, "supervised_auto", stats, (command, args, options) => pi.exec(command, args, options));
	return { handlers, calls, outcomes, ctx, stats };
}

test("direct write is mediated through the exact Core intent", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const { handlers, calls, ctx, stats } = harness(root);
		const result = await handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-1",
			input: { path: "source.txt", content: "changed" },
		}, ctx);
		assert.equal(result, undefined);
		assert.equal(stats.allowed, 1);
		const args = calls[0].args;
		assert.ok(args.includes("fs.write"));
		assert.ok(args.includes("workspace_write"));
		assert.ok(args.includes("write-1"));
		assert.ok(args.includes("source.txt"));
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("deny and unknown tool calls block before effects", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const denied = harness(root, "deny");
		const denial = await denied.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-denied",
			input: { path: "source.txt", content: "changed" },
		}, denied.ctx);
		assert.equal(denial.block, true);
		assert.equal(denied.stats.denied, 1);

		const unknown = harness(root);
		const blocked = await unknown.handlers.get("tool_call")?.[0]({
			toolName: "future_mutator",
			toolCallId: "unknown-1",
			input: {},
		}, unknown.ctx);
		assert.equal(blocked.block, true);
		assert.match(blocked.reason, /unsupported Pi tool/);
		assert.equal(unknown.calls.length, 0);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("gate execution uses a bounded sanitized environment without enforcement authority", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	const capture = path.join(root, "gate-env.txt");
	try {
		process.env.NOPAL_ENFORCEMENT_CAPABILITY = "must-not-leak";
		process.env.NOPAL_TEST_SECRET = "must-not-leak";
		const command = `env > ${JSON.stringify(capture)}`;
		const { handlers, ctx } = harness(root, "allow", false, command);
		const result = await handlers.get("tool_call")?.[0]({
			toolName: "bash",
			toolCallId: "gate-env",
			input: { command: "git push origin HEAD:refs/heads/main" },
		}, ctx);
		assert.equal(result, undefined);
		const captured = readFileSync(capture, "utf8");
		assert.match(captured, /^HOME=/m);
		assert.match(captured, /^PATH=\/trusted\/gate-bin:\/usr\/bin:\/bin$/m);
		assert.doesNotMatch(captured, /NOPAL_ENFORCEMENT|NOPAL_TEST_SECRET|must-not-leak/);
	} finally {
		delete process.env.NOPAL_ENFORCEMENT_CAPABILITY;
		delete process.env.NOPAL_TEST_SECRET;
		rmSync(root, { recursive: true, force: true });
	}
});

test("gate execution uses the run-private executor and ignores a writable ambient PATH shadow", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	const ambientBin = path.join(root, "ambient-bin");
	const trustedBin = path.join(root, "trusted-bin");
	const ambientMarker = path.join(root, "ambient-shadow-ran");
	const trustedMarker = path.join(root, "trusted-executor-ran");
	const originalPath = process.env.PATH;
	try {
		mkdirSync(ambientBin);
		mkdirSync(trustedBin);
		writeFileSync(path.join(ambientBin, "cargo"), `#!/bin/sh\ntouch ${JSON.stringify(ambientMarker)}\n`);
		writeFileSync(path.join(trustedBin, "cargo"), `#!/bin/sh\ntouch ${JSON.stringify(trustedMarker)}\n`);
		chmodSync(path.join(ambientBin, "cargo"), 0o755);
		chmodSync(path.join(trustedBin, "cargo"), 0o755);
		process.env.PATH = `${ambientBin}:${originalPath ?? ""}`;
		const { handlers, ctx } = harness(root, "allow", false, "cargo --version", false, trustedBin);
		const result = await handlers.get("tool_call")?.[0]({
			toolName: "bash",
			toolCallId: "shadow-gate",
			input: { command: "git push origin HEAD:refs/heads/main" },
		}, ctx);
		assert.equal(result, undefined);
		assert.equal(existsSync(trustedMarker), true);
		assert.equal(existsSync(ambientMarker), false);
	} finally {
		process.env.PATH = originalPath;
		rmSync(root, { recursive: true, force: true });
	}
});

test("ask approval is durably recorded and revalidated before release", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const { handlers, calls, ctx, stats } = harness(root, "ask", true);
		const result = await handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-ask",
			input: { path: "source.txt", content: "approved" },
		}, ctx);
		assert.equal(result, undefined);
		assert.equal(stats.asked, 1);
		assert.equal(stats.approved, 1);
		assert.ok(calls.some(({ args }) => args.includes("record-approval") && args.includes("--approved")));
		assert.ok(calls.some(({ args }) => args.includes("authorize")));
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("tool success, error, and shutdown interruption are durably recorded by release identity", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const successful = harness(root);
		await successful.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-success",
			input: { path: "source.txt", content: "changed" },
		}, successful.ctx);
		await successful.handlers.get("tool_result")?.[0]({
			toolName: "write",
			toolCallId: "write-success",
			input: { path: "source.txt", content: "changed" },
			content: [{ type: "text", text: "ok" }],
			isError: false,
		}, successful.ctx);
		assert.deepEqual(successful.outcomes, ["success"]);

		const failed = harness(root);
		await failed.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-error",
			input: { path: "source.txt", content: "changed" },
		}, failed.ctx);
		await failed.handlers.get("tool_result")?.[0]({
			toolName: "write",
			toolCallId: "write-error",
			input: { path: "source.txt", content: "changed" },
			content: [{ type: "text", text: "failed" }],
			isError: true,
		}, failed.ctx);
		assert.deepEqual(failed.outcomes, ["error"]);

		const cancelled = harness(root);
		await cancelled.handlers.get("tool_call")?.[0]({
			toolName: "read",
			toolCallId: "read-cancelled",
			input: { path: "source.txt" },
		}, cancelled.ctx);
		await cancelled.handlers.get("tool_result")?.[0]({
			toolName: "read",
			toolCallId: "read-cancelled",
			input: { path: "source.txt" },
			content: [{ type: "text", text: "Operation aborted" }],
			isError: true,
		}, cancelled.ctx);
		assert.deepEqual(cancelled.outcomes, ["cancelled"]);

		const ordinaryError = harness(root);
		await ordinaryError.handlers.get("tool_call")?.[0]({
			toolName: "bash",
			toolCallId: "bash-error",
			input: { command: "git status" },
		}, ordinaryError.ctx);
		await ordinaryError.handlers.get("tool_result")?.[0]({
			toolName: "bash",
			toolCallId: "bash-error",
			input: { command: "git status" },
			content: [{ type: "text", text: "Command aborted unexpectedly\n\nCommand exited with code 1" }],
			isError: true,
		}, ordinaryError.ctx);
		assert.deepEqual(ordinaryError.outcomes, ["error"]);

		const interrupted = harness(root);
		await interrupted.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-interrupted",
			input: { path: "source.txt", content: "changed" },
		}, interrupted.ctx);
		await interrupted.handlers.get("session_shutdown")?.[0]({}, interrupted.ctx);
		assert.deepEqual(interrupted.outcomes, ["interrupted"]);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("an unrecordable tool outcome poisons further authorization", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const guarded = harness(root, "allow", false, undefined, true);
		await guarded.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-unrecordable",
			input: { path: "source.txt", content: "changed" },
		}, guarded.ctx);
		await guarded.handlers.get("tool_result")?.[0]({
			toolName: "write",
			toolCallId: "write-unrecordable",
			input: { path: "source.txt", content: "changed" },
			content: [{ type: "text", text: "ok" }],
			isError: false,
		}, guarded.ctx);
		const blocked = await guarded.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-after-failure",
			input: { path: "source.txt", content: "changed" },
		}, guarded.ctx);
		assert.equal(blocked.block, true);
		assert.equal(guarded.stats.failClosed, 1);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("concurrent reads record independent outcomes and leave later mutation usable", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const guarded = harness(root);
		for (const toolCallId of ["read-a", "read-b"]) {
			assert.equal(await guarded.handlers.get("tool_call")?.[0]({
				toolName: "read",
				toolCallId,
				input: { path: `${toolCallId}.txt` },
			}, guarded.ctx), undefined);
		}
		for (const toolCallId of ["read-b", "read-a"]) {
			await guarded.handlers.get("tool_result")?.[0]({
				toolName: "read",
				toolCallId,
				input: { path: `${toolCallId}.txt` },
				content: [{ type: "text", text: "ok" }],
				isError: false,
			}, guarded.ctx);
		}
		assert.deepEqual(guarded.outcomes, ["success", "success"]);
		assert.equal(await guarded.handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-after-reads",
			input: { path: "source.txt", content: "changed" },
		}, guarded.ctx), undefined);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("concurrent gated reads require explicit non-mutating parallel-safe metadata", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const safe = harness(root, "allow", false, "true", false, "/trusted/gate-bin", {
			parallel_safe: true,
			mutates: false,
		});
		for (const toolCallId of ["safe-read-a", "safe-read-b"]) {
			assert.equal(await safe.handlers.get("tool_call")?.[0]({
				toolName: "read",
				toolCallId,
				input: { path: `${toolCallId}.txt` },
			}, safe.ctx), undefined);
		}
		for (const toolCallId of ["safe-read-a", "safe-read-b"]) {
			await safe.handlers.get("tool_result")?.[0]({
				toolName: "read",
				toolCallId,
				input: { path: `${toolCallId}.txt` },
				content: [{ type: "text", text: "ok" }],
				isError: false,
			}, safe.ctx);
		}
		assert.deepEqual(safe.outcomes, ["success", "success"]);

		for (const [name, metadata] of [
			["missing", {}],
			["not-parallel", { parallel_safe: false, mutates: false }],
			["mutating", { parallel_safe: true, mutates: true }],
			["autofix", { parallel_safe: true, mutates: false, autofix: "fix" }],
		] as const) {
			const guarded = harness(root, "allow", false, "true", false, "/trusted/gate-bin", metadata);
			assert.equal(await guarded.handlers.get("tool_call")?.[0]({
				toolName: "read",
				toolCallId: `${name}-a`,
				input: { path: "a.txt" },
			}, guarded.ctx), undefined, name);
			const blocked = await guarded.handlers.get("tool_call")?.[0]({
				toolName: "read",
				toolCallId: `${name}-b`,
				input: { path: "b.txt" },
			}, guarded.ctx);
			assert.equal(blocked.block, true, name);
			assert.match(blocked.reason, /overlapping read\/mutation|exclusive lease/, name);
		}
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("overlapping mutation or duplicate call IDs poison authorization until session shutdown", async () => {
	const root = mkdtempSync(path.join(os.tmpdir(), "nopal-guard-"));
	try {
		const { handlers, ctx } = harness(root);
		assert.equal(await handlers.get("tool_call")?.[0]({
			toolName: "read",
			toolCallId: "read-1",
			input: { path: "source.txt" },
		}, ctx), undefined);
		const sibling = await handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "read-1",
			input: { path: "source.txt", content: "changed" },
		}, ctx);
		assert.equal(sibling.block, true);
		assert.match(sibling.reason, /duplicate in-flight/);

		await handlers.get("tool_result")?.[0]({
			toolName: "read",
			toolCallId: "read-1",
			input: { path: "source.txt" },
			content: [{ type: "text", text: "ok" }],
			isError: false,
		}, ctx);
		const poisoned = await handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-3",
			input: { path: "source.txt", content: "changed" },
		}, ctx);
		assert.equal(poisoned.block, true);
		await handlers.get("session_shutdown")?.[0]({}, ctx);
		assert.equal(await handlers.get("tool_call")?.[0]({
			toolName: "write",
			toolCallId: "write-4",
			input: { path: "source.txt", content: "changed" },
		}, ctx), undefined);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
