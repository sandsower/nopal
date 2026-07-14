import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import * as subagentChildRuntime from "../subagent-child-runtime.ts";
import { loadSubagentRunnerModule } from "./setup.ts";

const subagentRunner = await loadSubagentRunnerModule<typeof import("../index.ts")>("../index.ts");

const stripAnsi = (value: string) => value.replace(/\[[0-9;]*m/g, "");

async function withTempAgentDir(fn: (dir: string) => Promise<void> | void) {
	const dir = mkdtempSync(join(tmpdir(), "subagent-runner-test-"));
	try {
		await fn(dir);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

function makeRunRecord(id: string, status: "running" | "success", paths: ReturnType<typeof subagentRunner.createSubagentArtifactPaths>) {
	return {
		id,
		label: id,
		mode: "headless" as const,
		cwd: "/repo",
		promptPreview: id,
		artifactDir: paths.artifactDir,
		promptPath: paths.promptPath,
		resultPath: paths.resultPath,
		transcriptPath: paths.transcriptPath,
		stderrPath: paths.stderrPath,
		metadataPath: paths.metadataPath,
		status,
		startedAt: "2026-04-27T08:30:00.000Z",
		completedAt: status === "success" ? "2026-04-27T08:32:00.000Z" : undefined,
	};
}

// ---------------------------------------------------------------------------
// subagent-runner helpers
// ---------------------------------------------------------------------------

test("subagent-runner helpers: sanitizes labels for artifact paths", () => {
	assert.equal(subagentRunner.sanitizeSubagentLabel("Scout Auth/API!  "), "scout-auth-api");
	assert.equal(subagentRunner.sanitizeSubagentLabel("..."), "subagent");
});

test("subagent-runner helpers: clamps timeout seconds", () => {
	assert.equal(subagentRunner.normalizeTimeoutSeconds(undefined), 300);
	assert.equal(subagentRunner.normalizeTimeoutSeconds(1), 5);
	assert.equal(subagentRunner.normalizeTimeoutSeconds(99999), 3600);
});

test("subagent-runner helpers: formats success report with artifact paths", () => {
	const report = subagentRunner.formatSubagentReport({
		status: "success",
		label: "scout",
		cwd: "/repo",
		prompt: "Do thing",
		result: "Done",
		artifactDir: "/tmp/subagents/scout",
		promptPath: "/tmp/subagents/scout/prompt.md",
		resultPath: "/tmp/subagents/scout/result.md",
		transcriptPath: "/tmp/subagents/scout/transcript.jsonl",
		stderrPath: "/tmp/subagents/scout/stderr.log",
		metadataPath: "/tmp/subagents/scout/metadata.json",
		startedAt: "2026-04-27T00:00:00.000Z",
		completedAt: "2026-04-27T00:00:01.000Z",
		exitCode: 0,
		timedOut: false,
	});

	assert.ok(report.includes("Subagent scout completed"));
	assert.ok(report.includes("Done"));
	assert.ok(report.includes("result: /tmp/subagents/scout/result.md"));
});

test("subagent-runner helpers: formats timeout as warning state without error label", () => {
	const report = subagentRunner.formatSubagentReport({
		status: "timeout",
		label: "scout",
		cwd: "/repo",
		prompt: "Do thing",
		result: "Subagent timed out after 300 seconds.\n\nPartial transcript tail:\nlatest output",
		artifactDir: "/tmp/subagents/scout",
		promptPath: "/tmp/subagents/scout/prompt.md",
		resultPath: "/tmp/subagents/scout/result.md",
		transcriptPath: "/tmp/subagents/scout/transcript.jsonl",
		stderrPath: "/tmp/subagents/scout/stderr.log",
		metadataPath: "/tmp/subagents/scout/metadata.json",
		startedAt: "2026-04-27T00:00:00.000Z",
		completedAt: "2026-04-27T00:05:00.000Z",
		exitCode: 143,
		timedOut: true,
		error: "Subagent timed out after 300 seconds",
	});

	assert.equal(subagentRunner.subagentNotificationLevel("timeout"), "warning");
	assert.equal(subagentRunner.subagentToolResultIsError("timeout"), true);
	assert.equal(subagentRunner.subagentToolResultIsError("killed"), true);
	assert.equal(subagentRunner.subagentToolResultIsError("error"), true);
	assert.equal(subagentRunner.subagentToolResultIsError("success"), false);
	assert.ok(report.includes("Subagent scout timed out"));
	assert.ok(report.includes("timed out after 300 seconds"));
	assert.ok(!report.includes("Error:"));
	const notification = subagentRunner.formatSubagentCompletionNotification({
		status: "success",
		label: "scout",
		cwd: "/repo",
		prompt: "Do thing",
		result: "Done",
		artifactDir: "/tmp/subagents/2026-04-27t00-00-00-000z-scout",
		promptPath: "/tmp/subagents/2026-04-27t00-00-00-000z-scout/prompt.md",
		resultPath: "/tmp/subagents/2026-04-27t00-00-00-000z-scout/result.md",
		transcriptPath: "/tmp/subagents/2026-04-27t00-00-00-000z-scout/transcript.jsonl",
		stderrPath: "/tmp/subagents/2026-04-27t00-00-00-000z-scout/stderr.log",
		metadataPath: "/tmp/subagents/2026-04-27t00-00-00-000z-scout/metadata.json",
		startedAt: "2026-04-27T00:00:00.000Z",
		completedAt: "2026-04-27T00:00:01.000Z",
		exitCode: 0,
		timedOut: false,
	});
	assert.ok(notification.includes("Full output: /subagents or subagent_result"));
	assert.ok(!notification.includes("transcript.jsonl"));
	assert.ok(!notification.includes("result.md"));
});

test("subagent-runner helpers: writes atomic JSON without leaving temp files", () =>
	withTempAgentDir((agentDir) => {
		const path = join(agentDir, "state.json");
		subagentRunner.writeAtomicJson(path, { ok: true });
		assert.deepEqual(JSON.parse(readFileSync(path, "utf-8")), { ok: true });
		assert.equal(
			readdirSync(agentDir).filter((name) => name.includes(".tmp")).length,
			0,
		);
	}));

test("subagent-runner helpers: post-exit stdio guard destroys streams that do not end", async () => {
	const handlers: Record<string, Function[]> = {};
	const stdoutHandlers: Record<string, Function[]> = {};
	const stderrHandlers: Record<string, Function[]> = {};
	let stdoutDestroyed = false;
	let stderrDestroyed = false;
	const on =
		(store: Record<string, Function[]>) =>
		(event: string, handler: Function) => {
			(store[event] ??= []).push(handler);
		};
	const emit = (store: Record<string, Function[]>, event: string) => {
		for (const handler of store[event] ?? []) handler();
	};
	const cleanup = subagentRunner.attachPostExitStdioGuard(
		{
			stdout: {
				on: on(stdoutHandlers),
				destroy: () => {
					stdoutDestroyed = true;
				},
			},
			stderr: {
				on: on(stderrHandlers),
				destroy: () => {
					stderrDestroyed = true;
				},
			},
			on: on(handlers),
		} as any,
		{ idleMs: 1, hardMs: 20 },
	);
	emit(handlers, "exit");
	await new Promise((resolve) => setTimeout(resolve, 5));
	assert.equal(stdoutDestroyed, true);
	assert.equal(stderrDestroyed, true);
	cleanup();
});

// ---------------------------------------------------------------------------
// subagent-runner command parser
// ---------------------------------------------------------------------------

test("subagent-runner command parser: parses plain prompt as headless", () => {
	assert.deepEqual(subagentRunner.parseSubagentCommandArgs("check repo"), { ok: true, prompt: "check repo", terminal: false });
});

test("subagent-runner command parser: parses terminal, label, timeout, and dash sentinel", () => {
	assert.deepEqual(subagentRunner.parseSubagentCommandArgs("--terminal --label qa --timeout 600 run checks"), {
		ok: true,
		prompt: "run checks",
		terminal: true,
		label: "qa",
		timeoutSeconds: 600,
	});
	assert.deepEqual(subagentRunner.parseSubagentCommandArgs("-- --prompt starts with dash"), { ok: true, prompt: "--prompt starts with dash", terminal: false });
});

test("subagent-runner command parser: rejects missing prompt and invalid timeout", () => {
	assert.equal(subagentRunner.parseSubagentCommandArgs("--terminal").ok, false);
	assert.equal(subagentRunner.parseSubagentCommandArgs("--timeout nope check").ok, false);
});

// ---------------------------------------------------------------------------
// subagent child runtime
// ---------------------------------------------------------------------------

test("subagent child runtime: injects child boundary instructions once", () => {
	const rewritten = subagentChildRuntime.rewriteChildSystemPrompt("base prompt");
	assert.ok(rewritten.includes("focused child subagent"));
	assert.ok(rewritten.includes("Do not propose, launch, or coordinate subagents"));
	assert.ok(rewritten.includes("Default policy: read-only"));
	assert.ok(rewritten.includes("base prompt"));
	assert.equal(subagentChildRuntime.rewriteChildSystemPrompt(rewritten), rewritten);
});

test("subagent child runtime: strips only known parent subagent tool artifacts", () => {
	const knownCall = { role: "assistant", content: [{ type: "text", text: "keep" }, { type: "toolCall", name: "subagent_start" }] };
	const knownResult = { role: "toolResult", toolName: "subagent_result", content: "done" };
	const unknownCall = { role: "assistant", content: [{ type: "toolCall", name: "read" }] };
	const unknownResult = { role: "toolResult", toolName: "read", content: "file" };
	const messages = [knownCall, knownResult, unknownCall, unknownResult];
	const stripped = subagentChildRuntime.stripParentSubagentArtifacts(messages);
	assert.equal(stripped.length, 3);
	assert.deepEqual(stripped[0], { role: "assistant", content: [{ type: "text", text: "keep" }] });
	assert.ok(stripped.includes(unknownCall));
	assert.ok(stripped.includes(unknownResult));
});

test("subagent child runtime: disables only known parent subagent tools from the active tool surface", () => {
	assert.deepEqual(
		subagentChildRuntime.activeToolNamesWithoutParentSubagents(["read", "subagent_start", "subagent_result", "subagent_list", "subagent_kill", "subagent_run", "mcp__linear__get_issue"]),
		["read", "mcp__linear__get_issue"],
	);
	assert.deepEqual(subagentChildRuntime.activeToolNamesWithoutParentSubagents([{ name: "read" }, { name: "subagent_start" }]), ["read"]);

	let activeTools = ["read", "subagent_start", "bash", "subagent_run"];
	const disabled = subagentChildRuntime.disableParentSubagentTools({
		getActiveTools: () => activeTools,
		setActiveTools: (names: string[]) => {
			activeTools = names;
		},
	} as any);
	assert.equal(disabled, true);
	assert.deepEqual(activeTools, ["read", "bash"]);
});

test("subagent child runtime: child runtime registers no tools or commands", () => {
	const tools: any[] = [];
	const commands: any[] = [];
	const handlers: Record<string, Function> = {};
	subagentChildRuntime.default({
		registerTool: (tool: any) => tools.push(tool),
		registerCommand: (name: string) => commands.push(name),
		on: (name: string, handler: Function) => {
			handlers[name] = handler;
			return () => {};
		},
	} as any);
	assert.equal(tools.length, 0);
	assert.equal(commands.length, 0);
	assert.notEqual(handlers.input, undefined);
	assert.notEqual(handlers.context, undefined);
	assert.notEqual(handlers.before_agent_start, undefined);
});

test("subagent child runtime: child runtime does not call action methods during extension loading", () => {
	const handlers: Record<string, Function> = {};
	assert.doesNotThrow(() =>
		subagentChildRuntime.default({
			getActiveTools: () => {
				throw new Error("actions unavailable during extension loading");
			},
			setActiveTools: () => {
				throw new Error("actions unavailable during extension loading");
			},
			on: (name: string, handler: Function) => {
				handlers[name] = handler;
				return () => {};
			},
		} as any),
	);
	assert.notEqual(handlers.session_start, undefined);
});

// ---------------------------------------------------------------------------
// subagent-runner registry
// ---------------------------------------------------------------------------

test("subagent-runner registry: upserts, completes, and lists subagent run records", () =>
	withTempAgentDir((agentDir) => {
		const record = subagentRunner.createSubagentRunRecord({
			agentDir,
			label: "Scout",
			cwd: "/repo",
			prompt: "Inspect the repository and summarize it",
			paths: subagentRunner.createSubagentArtifactPaths(agentDir, "scout", new Date("2026-04-27T08:30:00.000Z")),
			startedAt: "2026-04-27T08:30:00.000Z",
			pid: 123,
		});

		subagentRunner.upsertSubagentRunRecord(agentDir, record);
		let registry = subagentRunner.loadSubagentRegistry(agentDir);
		assert.equal(registry.runs.length, 1);
		assert.equal(registry.runs[0]?.status, "running");
		assert.equal(registry.runs[0]?.mode, "headless");
		assert.ok(registry.runs[0]?.promptPath.includes("prompt.md"));
		assert.ok(registry.runs[0]?.promptPreview.includes("Inspect the repository"));

		subagentRunner.completeSubagentRunRecord(agentDir, record.id, {
			status: "success",
			completedAt: "2026-04-27T08:31:00.000Z",
			exitCode: 0,
		});
		registry = subagentRunner.loadSubagentRegistry(agentDir);
		assert.equal(registry.runs[0]?.status, "success");
		assert.equal(registry.runs[0]?.exitCode, 0);
	}));

test("subagent-runner registry: cleans up kept terminal sessions and kills running terminal sessions", () =>
	withTempAgentDir(async (agentDir) => {
		const paths = subagentRunner.createSubagentArtifactPaths(agentDir, "term", new Date("2026-04-27T08:30:00.000Z"));
		const terminal = {
			backend: "tmux" as const,
			socketName: "pi-agents",
			sessionName: "pi-agent-term",
			attachCommand: "tmux -L pi-agents attach -t pi-agent-term",
			cleanupStatus: "kept" as const,
		};
		const record = subagentRunner.createSubagentRunRecord({ agentDir, label: "term", cwd: "/repo", prompt: "terminal", paths, startedAt: "2026-04-27T08:30:00.000Z", mode: "terminal", terminal });
		subagentRunner.upsertSubagentRunRecord(agentDir, { ...record, status: "error", completedAt: "2026-04-27T08:31:00.000Z" });
		const calls: any[] = [];
		const cleaned = await subagentRunner.cleanupWorkspaceTerminal(agentDir, record.id, async (invocation: any) => {
			calls.push(invocation);
			return { exitCode: 0, stdout: "", stderr: "" };
		});
		assert.equal(cleaned.ok, true);
		assert.deepEqual(calls[0].args, ["-L", "pi-agents", "kill-session", "-t", "pi-agent-term"]);
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.terminal?.cleanupStatus, "cleaned");

		const running = { ...record, id: "running-term", status: "running" as const, terminal: { ...terminal, cleanupStatus: "active" as const } };
		subagentRunner.upsertSubagentRunRecord(agentDir, running);
		const killed = await subagentRunner.killWorkspace(agentDir, running.id, async (invocation: any) => {
			calls.push(invocation);
			return { exitCode: 0, stdout: "", stderr: "" };
		});
		assert.equal(killed.ok, true);
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs.find((run) => run.id === running.id)?.status, "killed");
	}));

test("subagent-runner registry: formats running runs before recent completed runs", () =>
	withTempAgentDir((agentDir) => {
		const paths = subagentRunner.createSubagentArtifactPaths(agentDir, "a", new Date("2026-04-27T08:30:00.000Z"));
		subagentRunner.upsertSubagentRunRecord(agentDir, subagentRunner.createSubagentRunRecord({ agentDir, label: "done", cwd: "/repo", prompt: "done task", paths, startedAt: "2026-04-27T08:30:00.000Z" }));
		subagentRunner.completeSubagentRunRecord(agentDir, subagentRunner.loadSubagentRegistry(agentDir).runs[0]!.id, { status: "success", completedAt: "2026-04-27T08:31:00.000Z" });
		const runningPaths = subagentRunner.createSubagentArtifactPaths(agentDir, "b", new Date("2026-04-27T08:32:00.000Z"));
		subagentRunner.upsertSubagentRunRecord(
			agentDir,
			subagentRunner.createSubagentRunRecord({ agentDir, label: "running", cwd: "/repo", prompt: "running task", paths: runningPaths, startedAt: "2026-04-27T08:32:00.000Z" }),
		);

		const report = subagentRunner.formatSubagentRuns(subagentRunner.listSubagentRuns(agentDir));
		assert.ok(report.indexOf("running") < report.indexOf("done"));
		assert.ok(report.includes("Artifacts:"));
	}));

test("subagent-runner registry: normalizes legacy records and dismisses records without deleting artifacts", () =>
	withTempAgentDir((agentDir) => {
		const paths = subagentRunner.createSubagentArtifactPaths(agentDir, "legacy", new Date("2026-04-27T08:30:00.000Z"));
		mkdirSync(paths.artifactDir, { recursive: true });
		writeFileSync(paths.resultPath, "kept artifact", "utf-8");
		writeFileSync(
			join(agentDir, "subagents", "runs.json"),
			`${JSON.stringify(
				{
					runs: [
						{
							id: "legacy",
							label: "legacy",
							cwd: "/repo",
							promptPreview: "old run",
							artifactDir: paths.artifactDir,
							resultPath: paths.resultPath,
							transcriptPath: paths.transcriptPath,
							stderrPath: paths.stderrPath,
							metadataPath: paths.metadataPath,
							status: "success",
							startedAt: "2026-04-27T08:30:00.000Z",
						},
					],
				},
				null,
				2,
			)}\n`,
			"utf-8",
		);

		const legacy = subagentRunner.loadSubagentRegistry(agentDir).runs[0]!;
		assert.equal(legacy.mode, "headless");
		assert.equal(legacy.promptPath, paths.promptPath);

		subagentRunner.dismissSubagentRunRecord(agentDir, "legacy");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs.length, 0);
		assert.equal(readFileSync(paths.resultPath, "utf-8"), "kept artifact");
	}));

test("subagent-runner registry: reconstructs prompt preview from metadata audit field when registry is missing", () =>
	withTempAgentDir((agentDir) => {
		const paths = subagentRunner.createSubagentArtifactPaths(agentDir, "metadata", new Date("2026-04-27T08:30:00.000Z"));
		mkdirSync(paths.artifactDir, { recursive: true });
		writeFileSync(
			paths.metadataPath,
			`${JSON.stringify(
				{
					label: "metadata",
					mode: "headless",
					cwd: "/repo",
					status: "success",
					startedAt: "2026-04-27T08:30:00.000Z",
					originalPromptPreview: "preview from audit metadata",
				},
				null,
				2,
			)}\n`,
			"utf-8",
		);

		const run = subagentRunner.findSubagentRun(agentDir, paths.artifactDir.split("/").at(-1)!);
		assert.equal(run?.promptPreview, "preview from audit metadata");
	}));

test("subagent-runner registry: dismisses all completed records while preserving running runs and artifacts", () =>
	withTempAgentDir((agentDir) => {
		const donePaths = subagentRunner.createSubagentArtifactPaths(agentDir, "done", new Date("2026-04-27T08:30:00.000Z"));
		const runningPaths = subagentRunner.createSubagentArtifactPaths(agentDir, "running", new Date("2026-04-27T08:31:00.000Z"));
		mkdirSync(donePaths.artifactDir, { recursive: true });
		writeFileSync(donePaths.resultPath, "kept artifact", "utf-8");
		subagentRunner.upsertSubagentRunRecord(agentDir, makeRunRecord("done", "success", donePaths));
		subagentRunner.upsertSubagentRunRecord(agentDir, makeRunRecord("running", "running", runningPaths));

		assert.equal(subagentRunner.dismissCompletedSubagentRunRecords(agentDir), 1);
		assert.deepEqual(
			subagentRunner.loadSubagentRegistry(agentDir).runs.map((run: any) => run.id),
			["running"],
		);
		assert.equal(readFileSync(donePaths.resultPath, "utf-8"), "kept artifact");
	}));

// ---------------------------------------------------------------------------
// subagent-runner tmux backend
// ---------------------------------------------------------------------------

test("subagent-runner tmux backend: builds tmux metadata and detached new-session invocation", () => {
	const paths = subagentRunner.createSubagentArtifactPaths("/agent", "qa", new Date("2026-04-27T08:30:00.000Z"));
	const spec = subagentRunner.createTmuxWorkspaceSpec({
		id: "2026-04-27t08-30-00-000z-qa",
		cwd: "/repo path",
		prompt: "check repo",
		paths,
	});
	assert.equal(spec.terminal.socketName, "pi-agents");
	assert.equal(spec.terminal.sessionName, "pi-agent-2026-04-27t08-30-00-000z-qa".slice(0, 48));
	assert.ok(spec.terminal.attachCommand.includes("tmux -L pi-agents attach -t"));
	assert.equal(spec.start.command, "tmux");
	assert.deepEqual(spec.start.args.slice(0, 5), ["-L", "pi-agents", "new-session", "-d", "-s"]);
	const shellCommand = spec.start.args.join(" ");
	assert.ok(shellCommand.includes(spec.terminal.sessionName));
	assert.ok(shellCommand.includes("PI_SUBAGENT_CHILD='1'"));
	assert.ok(shellCommand.includes("--extension"));
	assert.ok(shellCommand.includes(`@${paths.promptPath}`));
	assert.ok(spec.exitCodePath.includes("exit-code.txt"));
});

// ---------------------------------------------------------------------------
// subagent-runner workspace UI
// ---------------------------------------------------------------------------

const baseRun = {
	id: "2026-04-27t08-30-00-000z-manual",
	label: "manual",
	mode: "headless" as const,
	cwd: "/repo",
	promptPreview: "inspect repo extension structure and summarize important details",
	artifactDir: "/tmp/a",
	promptPath: "/tmp/a/prompt.md",
	resultPath: "/tmp/a/result.md",
	transcriptPath: "/tmp/a/transcript.jsonl",
	stderrPath: "/tmp/a/stderr.log",
	metadataPath: "/tmp/a/metadata.json",
	status: "running" as const,
	startedAt: "2026-04-27T08:30:00.000Z",
};

test("subagent-runner workspace UI: renders compact list rows with status and mode markers", () => {
	const lines = subagentRunner.renderSubagentListLines([baseRun], { selectedIndex: 0, width: 90, now: Date.parse("2026-04-27T08:32:13.000Z") });
	assert.ok(lines[0]?.includes("Subagents"));
	assert.equal(lines[0]?.startsWith("┌"), true);
	assert.equal(lines.at(-1)?.startsWith("└"), true);
	assert.ok(lines.some((line: string) => line.includes("> ● h")));
	assert.ok(lines.some((line: string) => line.includes("manual")));
	assert.ok(lines.some((line: string) => line.includes("inspect repo")));
	assert.ok(lines.some((line: string) => line.includes("D dismiss completed")));
	assert.ok(lines.every((line: string) => stripAnsi(line).length <= 90));
});

test("subagent-runner workspace UI: keeps list and detail rendering within visible width", () => {
	const longRun = {
		...baseRun,
		label: "very-long-label-that-should-not-overflow",
		promptPreview: `[31m${"inspect ".repeat(30)}[0m`,
		cwd: "/very/long/path/that/should/be/shortened/in/the/detail/view/repository",
		artifactDir: "/very/long/path/that/should/be/shortened/in/the/detail/view/artifact",
		promptPath: "/very/long/path/that/should/be/shortened/in/the/detail/view/artifact/prompt.md",
	};
	const listLines = subagentRunner.renderSubagentListLines([longRun], { selectedIndex: 0, width: 48, now: Date.parse("2026-04-27T08:32:13.000Z") });
	const detailLines = subagentRunner.renderSubagentDetailLines(longRun, { width: 48, now: Date.parse("2026-04-27T08:32:13.000Z") });
	assert.ok(listLines.every((line: string) => stripAnsi(line).length <= 48));
	assert.ok(detailLines.every((line: string) => stripAnsi(line).length <= 48));
});

test("subagent-runner workspace UI: renders a scroll window that keeps the selected run visible", () => {
	const runs = Array.from({ length: 8 }, (_, index) => ({
		...baseRun,
		id: `2026-04-27t08-3${index}-00-000z-run-${index}`,
		label: `run-${index}`,
		promptPreview: `prompt ${index}`,
		startedAt: `2026-04-27T08:3${index}:00.000Z`,
	}));
	const lines = subagentRunner.renderSubagentListLines(runs, { selectedIndex: 6, width: 90, maxRows: 3, now: Date.parse("2026-04-27T08:40:00.000Z") });
	const text = lines.join("\n");
	assert.ok(text.includes("showing 5-7 of 8"));
	assert.ok(text.includes("run-6"));
	assert.ok(!text.includes("run-0"));
});

test("subagent-runner workspace UI: updates navigation state and emits dismiss action", () => {
	const runs = [baseRun, { ...baseRun, id: "done", status: "success" as const, completedAt: "2026-04-27T08:31:00.000Z" }];
	let next = subagentRunner.reduceSubagentUiState({ selectedIndex: 0, detail: false }, "j", runs);
	assert.equal(next.state.selectedIndex, 1);
	next = subagentRunner.reduceSubagentUiState(next.state, "[A", runs);
	assert.equal(next.state.selectedIndex, 0);
	next = subagentRunner.reduceSubagentUiState(next.state, "\r", runs);
	assert.equal(next.state.detail, true);
	next = subagentRunner.reduceSubagentUiState(next.state, "escape", runs);
	assert.equal(next.state.detail, false);
	next = subagentRunner.reduceSubagentUiState({ selectedIndex: 1, detail: false }, "d", runs);
	assert.deepEqual(next.action, { type: "dismiss", id: "done" });
	next = subagentRunner.reduceSubagentUiState({ selectedIndex: 0, detail: false }, "d", runs);
	assert.equal(next.action, undefined);
});

test("subagent-runner workspace UI: confirms dismissing all completed runs", () => {
	const runs = [baseRun, { ...baseRun, id: "done", status: "success" as const, completedAt: "2026-04-27T08:31:00.000Z" }];
	let next = subagentRunner.reduceSubagentUiState({ selectedIndex: 0, detail: false }, "D", runs);
	assert.equal(next.state.confirm?.type, "dismiss-completed");
	next = subagentRunner.reduceSubagentUiState(next.state, "y", runs);
	assert.deepEqual(next.action, { type: "dismiss-completed" });
});

test("subagent-runner workspace UI: confirms kill and cleanup actions", () => {
	const terminalRun = {
		...baseRun,
		mode: "terminal" as const,
		terminal: { backend: "tmux" as const, socketName: "pi-agents", sessionName: "pi-agent-x", attachCommand: "tmux attach", cleanupStatus: "kept" as const },
	};
	let next = subagentRunner.reduceSubagentUiState({ selectedIndex: 0, detail: false }, "K", [terminalRun]);
	assert.equal(next.state.confirm?.type, "kill");
	next = subagentRunner.reduceSubagentUiState(next.state, "y", [terminalRun]);
	assert.deepEqual(next.action, { type: "kill", id: terminalRun.id });

	const kept = { ...terminalRun, status: "error" as const };
	next = subagentRunner.reduceSubagentUiState({ selectedIndex: 0, detail: false }, "c", [kept]);
	assert.equal(next.state.confirm?.type, "cleanup");
	next = subagentRunner.reduceSubagentUiState(next.state, "n", [kept]);
	assert.equal(next.action, undefined);
	assert.equal(next.state.confirm, undefined);
});

test("subagent-runner workspace UI: renders detail lines with artifacts and tails", () =>
	withTempAgentDir((agentDir) => {
		const paths = subagentRunner.createSubagentArtifactPaths(agentDir, "detail", new Date("2026-04-27T08:30:00.000Z"));
		mkdirSync(paths.artifactDir, { recursive: true });
		writeFileSync(paths.resultPath, "final result\n", "utf-8");
		writeFileSync(paths.transcriptPath, "line1\nline2\n", "utf-8");
		writeFileSync(paths.stderrPath, "warn1\nwarn2\n", "utf-8");
		writeFileSync(paths.metadataPath, `${JSON.stringify({ status: "success", promptSource: "artifact-file", promptPath: paths.promptPath }, null, 2)}\n`, "utf-8");
		const run = { ...baseRun, ...paths, artifactDir: paths.artifactDir, status: "success" as const };
		const lines = subagentRunner.renderSubagentDetailLines(run, { width: 100, now: Date.parse("2026-04-27T08:32:13.000Z") });
		const text = lines.join("\n");
		assert.ok(text.includes("mode: headless"));
		assert.ok(text.includes("prompt.md"));
		assert.ok(text.includes("final result"));
		assert.ok(text.includes("line2"));
		assert.ok(text.includes("warn2"));
		assert.ok(text.includes("Metadata tail"));
		assert.ok(text.includes("artifact-file"));
	}));

test("subagent-runner workspace UI: renders terminal attach and cleanup hints", () => {
	const terminalRun = {
		...baseRun,
		mode: "terminal" as const,
		status: "error" as const,
		terminal: { backend: "tmux" as const, socketName: "pi-agents", sessionName: "pi-agent-x", attachCommand: "tmux -L pi-agents attach -t pi-agent-x", cleanupStatus: "kept" as const },
	};
	const text = subagentRunner.renderSubagentDetailLines(terminalRun, { width: 100 }).join("\n");
	assert.ok(text.includes("attach: tmux -L pi-agents attach -t pi-agent-x"));
	assert.ok(text.includes("cleanup available"));
});

test("subagent-runner workspace UI: renders running subagents widget and clears when idle", () => {
	const lines = subagentRunner.renderRunningSubagentsWidget(
		[baseRun, { ...baseRun, id: "done", label: "done", status: "success" as const, completedAt: "2026-04-27T08:31:00.000Z" }],
		{ width: 60, now: Date.parse("2026-04-27T08:32:13.000Z") },
	);
	assert.equal(lines?.[0], "subagents: 1 running");
	assert.ok(lines?.join("\n").includes("manual"));
	assert.ok(lines?.every((line: string) => stripAnsi(line).length <= 60));
	const narrowLines = subagentRunner.renderRunningSubagentsWidget([baseRun], { width: 10, now: Date.parse("2026-04-27T08:32:13.000Z") });
	assert.ok(narrowLines?.every((line: string) => stripAnsi(line).length <= 10));
	assert.equal(subagentRunner.renderRunningSubagentsWidget([{ ...baseRun, status: "success" as const }]), undefined);
});

test("subagent-runner workspace UI: overlay refresh helper requests renders and clears its interval", () => {
	let tick: (() => void) | undefined;
	let cleared = false;
	let renders = 0;
	let updates = 0;
	const stop = subagentRunner.startSubagentsOverlayRefresh(
		{
			requestRender: () => {
				renders += 1;
			},
			updateUi: () => {
				updates += 1;
			},
		},
		{
			setInterval: (fn: () => void, _ms: number) => {
				tick = fn;
				return 123 as any;
			},
			clearInterval: (timer: any) => {
				if (timer === 123) cleared = true;
			},
		},
	);
	tick?.();
	assert.equal(renders, 1);
	assert.equal(updates, 1);
	stop();
	assert.equal(cleared, true);
});

// ---------------------------------------------------------------------------
// subagent-runner execution
// ---------------------------------------------------------------------------

test("subagent-runner execution: starts a workspace without waiting for completion and polls running result", () =>
	withTempAgentDir(async (agentDir) => {
		let release!: () => void;
		const workspace = subagentRunner.startSubagentWorkspace({ prompt: "Scout slowly", label: "slow", agentDir, now: new Date("2026-04-27T08:30:00.000Z") }, async (_request, _signal, lifecycle) => {
			lifecycle?.onStarted?.({ invocation: { command: "fake-pi", args: [] } });
			await new Promise<void>((resolve) => {
				release = resolve;
			});
			return { exitCode: 0, stdout: "final", stderr: "", result: "final", invocation: { command: "fake-pi", args: [] } };
		});

		const started = await workspace.started;
		assert.equal(started.status, "started");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.status, "running");

		const runningResult = await subagentRunner.readSubagentResult(agentDir, started.id, { block: false });
		assert.equal(runningResult.retrieval_status, "not_ready");
		assert.equal(runningResult.run?.status, "running");
		assert.equal(runningResult.run?.artifactDir, started.artifactDir);

		release();
		const completed = await workspace.completion;
		assert.equal(completed.status, "success");
		assert.equal(readFileSync(completed.resultPath, "utf-8"), "final\n");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.status, "success");
	}));

test("subagent-runner execution: rejects starting a fifth running workspace with capacity summary", () =>
	withTempAgentDir(async (agentDir) => {
		for (let i = 0; i < 4; i++) {
			const paths = subagentRunner.createSubagentArtifactPaths(agentDir, `run-${i}`, new Date(`2026-04-27T08:3${i}:00.000Z`));
			subagentRunner.upsertSubagentRunRecord(
				agentDir,
				subagentRunner.createSubagentRunRecord({ agentDir, label: `run-${i}`, cwd: "/repo", prompt: `prompt ${i}`, paths, startedAt: `2026-04-27T08:3${i}:00.000Z`, mode: i === 0 ? "terminal" : "headless" }),
			);
		}

		assert.throws(() => subagentRunner.startSubagentWorkspace({ prompt: "too many", agentDir }), /4 running/);
		try {
			subagentRunner.startSubagentWorkspace({ prompt: "too many", agentDir });
			assert.fail("expected startSubagentWorkspace to throw");
		} catch (error) {
			assert.ok(String(error).includes("run-0"));
			assert.ok(String(error).includes("prompt 0"));
		}
	}));

test("subagent-runner execution: failed start records durable error and started payload", () =>
	withTempAgentDir(async (agentDir) => {
		const workspace = subagentRunner.startSubagentWorkspace({ prompt: "Cannot spawn", label: "spawn-fail", agentDir, now: new Date("2026-04-27T08:30:00.000Z") }, async () => {
			throw new Error("spawn ENOENT");
		});

		const started = await workspace.started;
		assert.equal(started.status, "failed_to_start");
		assert.ok(started.error?.includes("spawn ENOENT"));
		const completed = await workspace.completion;
		assert.equal(completed.status, "error");
		const run = subagentRunner.loadSubagentRegistry(agentDir).runs[0]!;
		assert.equal(run.status, "error");
		assert.ok(run.error?.includes("spawn ENOENT"));
		assert.ok(JSON.parse(readFileSync(run.metadataPath, "utf-8")).error.includes("spawn ENOENT"));
	}));

test("subagent-runner execution: runs a fake subagent and writes artifacts", () =>
	withTempAgentDir(async (agentDir) => {
		const result = await subagentRunner.runSubagentTask(
			{
				prompt: "Summarize the repo",
				label: "Repo Scout",
				cwd: "/repo",
				now: new Date("2026-04-27T08:30:00.000Z"),
				agentDir,
			},
			async (request) => ({
				exitCode: 0,
				stdout: "Repository summary",
				stderr: "",
				result: "Repository summary",
				transcript: '{"type":"message_end"}\n',
				invocation: { command: "fake-pi", args: [request.prompt] },
			}),
		);

		assert.equal(result.status, "success");
		assert.equal(result.result, "Repository summary");
		assert.ok(result.artifactDir.includes("2026-04-27t08-30-00-000z-repo-scout"));
		assert.ok(readFileSync(result.promptPath, "utf-8").includes("Summarize the repo"));
		assert.equal(readFileSync(result.resultPath, "utf-8"), "Repository summary\n");
		assert.ok(readFileSync(result.transcriptPath, "utf-8").includes("message_end"));
		assert.equal(existsSync(result.stderrPath), true);
		const metadata = JSON.parse(readFileSync(result.metadataPath, "utf-8"));
		assert.equal(metadata.status, "success");
		assert.equal(metadata.exitCode, 0);
		assert.equal(metadata.promptSource, "artifact-file");
		assert.equal(metadata.promptPath, result.promptPath);
		assert.ok(metadata.originalPromptPreview.includes("Summarize the repo"));
	}));

test("subagent-runner execution: rejects empty prompts", () =>
	withTempAgentDir(async (agentDir) => {
		await assert.rejects(
			subagentRunner.runSubagentTask({ prompt: "   ", agentDir }, async () => ({ exitCode: 0, stdout: "", stderr: "" })),
			/Subagent prompt is required/,
		);
	}));

test("subagent-runner execution: writes running metadata and registry before subagent resolves", () =>
	withTempAgentDir(async (agentDir) => {
		let release!: () => void;
		const running = subagentRunner.runSubagentTask({ prompt: "Slow", label: "slow", agentDir, now: new Date("2026-04-27T08:30:00.000Z") }, async (request) => {
			const metadata = JSON.parse(readFileSync(join(request.artifactDir, "metadata.json"), "utf-8"));
			assert.equal(metadata.status, "running");
			assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.status, "running");
			await new Promise<void>((resolve) => {
				release = resolve;
			});
			return { exitCode: 0, stdout: "done", stderr: "", result: "done", invocation: { command: "fake-pi", args: [] } };
		});

		await Promise.resolve();
		release();
		const result = await running;
		assert.equal(result.status, "success");
		assert.equal(JSON.parse(readFileSync(result.metadataPath, "utf-8")).status, "success");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.status, "success");
	}));

test("subagent-runner execution: records non-zero exit as error", () =>
	withTempAgentDir(async (agentDir) => {
		const result = await subagentRunner.runSubagentTask({ prompt: "Fail", label: "fail", agentDir, now: new Date("2026-04-27T08:30:00.000Z") }, async () => ({
			exitCode: 2,
			stdout: "partial",
			stderr: "boom",
			invocation: { command: "fake-pi", args: [] },
		}));

		assert.equal(result.status, "error");
		assert.equal(result.exitCode, 2);
		assert.equal(readFileSync(result.stderrPath, "utf-8"), "boom");
		assert.ok(JSON.parse(readFileSync(result.metadataPath, "utf-8")).error.includes("exited with code 2"));
	}));

test("subagent-runner execution: records terminal metadata and cleanup status", () =>
	withTempAgentDir(async (agentDir) => {
		const success = await subagentRunner.runSubagentTask({ prompt: "Terminal", label: "term", agentDir, terminal: true, now: new Date("2026-04-27T08:30:00.000Z") }, async () => ({
			exitCode: 0,
			stdout: "ok",
			stderr: "",
			result: "ok",
			invocation: { command: "fake-terminal", args: [] },
		}));
		let run = subagentRunner.loadSubagentRegistry(agentDir).runs.find((candidate) => candidate.id === success.artifactDir.split("/").at(-1));
		assert.equal(run?.mode, "terminal");
		assert.ok(run?.terminal?.attachCommand.includes("tmux -L pi-agents attach -t"));
		assert.equal(run?.terminal?.cleanupStatus, "cleaned");

		const failure = await subagentRunner.runSubagentTask({ prompt: "Terminal fail", label: "term-fail", agentDir, terminal: true, now: new Date("2026-04-27T08:31:00.000Z") }, async () => ({
			exitCode: 1,
			stdout: "bad",
			stderr: "",
			result: "bad",
			invocation: { command: "fake-terminal", args: [] },
		}));
		run = subagentRunner.loadSubagentRegistry(agentDir).runs.find((candidate) => candidate.id === failure.artifactDir.split("/").at(-1));
		assert.equal(run?.status, "error");
		assert.equal(run?.terminal?.cleanupStatus, "kept");
	}));

test("subagent-runner execution: kills active headless workspace through active handle map", () =>
	withTempAgentDir(async (agentDir) => {
		let aborted = false;
		const running = subagentRunner.runSubagentTask({ prompt: "Hang", label: "kill-me", agentDir, timeoutSeconds: 30, now: new Date("2026-04-27T08:30:00.000Z") }, async (_request, signal) =>
			new Promise((resolve) => {
				signal?.addEventListener("abort", () => {
					aborted = true;
					resolve({ exitCode: 1, stdout: "", stderr: "killed" });
				});
			}),
		);
		await Promise.resolve();
		const id = subagentRunner.loadSubagentRegistry(agentDir).runs[0]!.id;
		const killed = await subagentRunner.killWorkspace(agentDir, id);
		assert.equal(killed.ok, true);
		const result = await running;
		assert.equal(aborted, true);
		assert.equal(result.status, "killed");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.status, "killed");
	}));

test("subagent-runner execution: terminal kill aborts active run and preserves cleaned killed status", () =>
	withTempAgentDir(async (agentDir) => {
		let aborted = false;
		const running = subagentRunner.runSubagentTask(
			{ prompt: "Terminal hang", label: "term-kill", agentDir, terminal: true, timeoutSeconds: 30, now: new Date("2026-04-27T08:30:00.000Z") },
			async (_request, signal) =>
				new Promise((resolve) => {
					signal?.addEventListener("abort", () => {
						aborted = true;
						resolve({ exitCode: 1, stdout: "", stderr: "killed" });
					});
				}),
		);
		await Promise.resolve();
		const id = subagentRunner.loadSubagentRegistry(agentDir).runs[0]!.id;
		const killed = await subagentRunner.killWorkspace(agentDir, id, async () => ({ exitCode: 0, stdout: "", stderr: "" }));
		assert.equal(killed.ok, true);
		const result = await running;
		assert.equal(aborted, true);
		assert.equal(result.status, "killed");
		const record = subagentRunner.loadSubagentRegistry(agentDir).runs[0]!;
		assert.equal(record.status, "killed");
		assert.equal(record.terminal?.cleanupStatus, "cleaned");
	}));

test("subagent-runner execution: terminal timeout records cleanup status from runner", () =>
	withTempAgentDir(async (agentDir) => {
		const result = await subagentRunner.runSubagentTask(
			{ prompt: "Terminal timeout", label: "term-timeout", agentDir, terminal: true, timeoutSeconds: 5, now: new Date("2026-04-27T08:30:00.000Z") },
			async (_request, signal) =>
				new Promise((resolve) => {
					signal?.addEventListener("abort", () => resolve({ exitCode: 1, stdout: "", stderr: "timeout", terminalCleanupStatus: "failed" }));
				}),
			{
				setTimeout: (fn) => {
					fn();
					return 1 as unknown as ReturnType<typeof setTimeout>;
				},
				clearTimeout: () => {},
			},
		);
		assert.equal(result.status, "timeout");
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.terminal?.cleanupStatus, "failed");
	}));

test("subagent-runner execution: records timeout and aborts fake runner", () =>
	withTempAgentDir(async (agentDir) => {
		let aborted = false;
		const partialTranscript = [
			JSON.stringify({ type: "tool_execution_update", partialResult: { content: [{ type: "text", text: "first output" }] } }),
			JSON.stringify({ type: "tool_execution_update", partialResult: { content: [{ type: "text", text: "latest partial output" }] } }),
		].join("\n");
		const result = await subagentRunner.runSubagentTask(
			{ prompt: "Hang", label: "hang", agentDir, timeoutSeconds: 5, now: new Date("2026-04-27T08:30:00.000Z") },
			async (_request, signal) =>
				new Promise((resolve) => {
					signal?.addEventListener("abort", () => {
						aborted = true;
						resolve({ exitCode: 1, stdout: partialTranscript, stderr: "aborted" });
					});
				}),
			{
				setTimeout: (fn) => {
					fn();
					return 1 as unknown as ReturnType<typeof setTimeout>;
				},
				clearTimeout: () => {},
			},
		);

		assert.equal(aborted, true);
		assert.equal(result.status, "timeout");
		assert.equal(result.timedOut, true);
		assert.ok(result.result?.includes("Partial transcript tail"));
		assert.ok(result.result?.includes("latest partial output"));
		assert.ok(readFileSync(result.resultPath, "utf-8").includes("latest partial output"));
		assert.equal(JSON.parse(readFileSync(result.metadataPath, "utf-8")).status, "timeout");
	}));

// ---------------------------------------------------------------------------
// subagent-runner pi invocation and registration
// ---------------------------------------------------------------------------

test("subagent-runner pi invocation and registration: builds one-shot pi json invocation from prompt artifact", () => {
	const invocation = subagentRunner.buildPiSubprocessInvocation("/tmp/run/prompt.md");
	assert.ok(invocation.args.includes("--mode"));
	assert.ok(invocation.args.includes("json"));
	assert.ok(invocation.args.includes("-p"));
	assert.ok(invocation.args.includes("--no-session"));
	assert.ok(invocation.args.includes("--extension"));
	assert.ok(invocation.args.some((arg: string) => arg.endsWith("subagent-child-runtime.ts")));
	assert.equal(invocation.args.at(-1), "@/tmp/run/prompt.md");
	assert.equal(invocation.env?.PI_SUBAGENT_CHILD, "1");
});

test("subagent-runner pi invocation and registration: registers runner tools and commands without role-layer surface", () => {
	const tools: Record<string, any> = {};
	const commands: Record<string, any> = {};
	subagentRunner.default(
		{
			registerTool: (tool: any) => {
				tools[tool.name] = tool;
			},
			registerCommand: (name: string, command: any) => {
				commands[name] = command;
			},
		} as any,
		async () => ({ exitCode: 0, stdout: "ok", stderr: "", result: "ok" }),
	);

	assert.deepEqual(Object.keys(tools).sort(), ["subagent_kill", "subagent_list", "subagent_result", "subagent_run", "subagent_start"]);
	assert.ok(tools.subagent_run.label.includes("Wait"));
	assert.equal(tools.subagent_run.parameters.properties.terminal.type, "boolean");
	assert.notEqual(commands.subagent, undefined);
	assert.notEqual(commands.subagents, undefined);
	assert.equal(tools.subagent, undefined);
	for (const name of ["agents", "run", "chain", "parallel", "run-chain", "subagents-status"]) {
		assert.equal(commands[name], undefined);
	}
});

test("subagent-runner pi invocation and registration: registers no parent tools or commands in child env", () => {
	const previous = process.env.PI_SUBAGENT_CHILD;
	process.env.PI_SUBAGENT_CHILD = "1";
	try {
		const tools: Record<string, any> = {};
		const commands: Record<string, any> = {};
		subagentRunner.default(
			{
				registerTool: (tool: any) => {
					tools[tool.name] = tool;
				},
				registerCommand: (name: string, command: any) => {
					commands[name] = command;
				},
			} as any,
			async () => ({ exitCode: 0, stdout: "ok", stderr: "", result: "ok" }),
		);
		assert.equal(Object.keys(tools).length, 0);
		assert.equal(Object.keys(commands).length, 0);
	} finally {
		if (previous === undefined) delete process.env.PI_SUBAGENT_CHILD;
		else process.env.PI_SUBAGENT_CHILD = previous;
	}
});

test("subagent-runner pi invocation and registration: subagent_start tool returns before completion and result tool can join", () =>
	withTempAgentDir(async (agentDir) => {
		const tools: Record<string, any> = {};
		let release!: () => void;
		subagentRunner.default(
			{
				registerTool: (tool: any) => {
					tools[tool.name] = tool;
				},
				registerCommand: () => {},
			} as any,
			async (_request, _signal, lifecycle) => {
				lifecycle?.onStarted?.({ invocation: { command: "fake-pi", args: [] } });
				await new Promise<void>((resolve) => {
					release = resolve;
				});
				return { exitCode: 0, stdout: "done", stderr: "", result: "done" };
			},
		);

		const started = await tools.subagent_start.execute("tc", { prompt: "check" }, undefined, undefined, { cwd: "/repo", ui: { setStatus: () => {}, notify: () => {} }, agentDir });
		assert.equal(started.isError, false);
		assert.ok(started.content[0].text.includes("running in the background"));
		const id = started.details.id;
		const poll = await tools.subagent_result.execute("tc", { id, block: false }, undefined, undefined, { cwd: "/repo", ui: {}, agentDir });
		assert.equal(poll.details.retrieval_status, "not_ready");
		release();
		const joined = await tools.subagent_result.execute("tc", { id, block: true, timeoutMs: 1000 }, undefined, undefined, { cwd: "/repo", ui: {}, agentDir });
		assert.equal(joined.details.retrieval_status, "success");
		assert.equal(joined.details.run.result, "done");
	}));

test("subagent-runner pi invocation and registration: tool terminal true creates terminal workspace", () =>
	withTempAgentDir(async (agentDir) => {
		const tools: Record<string, any> = {};
		let capturedRequest: any;
		subagentRunner.default(
			{
				registerTool: (tool: any) => {
					tools[tool.name] = tool;
				},
				registerCommand: () => {},
			} as any,
			async (request) => {
				capturedRequest = request;
				return { exitCode: 0, stdout: "ok", stderr: "", result: "ok" };
			},
		);

		const result = await tools.subagent_run.execute("tc", { prompt: "check", terminal: true }, undefined, undefined, {
			cwd: "/repo",
			ui: { setStatus: () => {} },
			agentDir,
		});
		assert.equal(result.isError, false);
		assert.ok(capturedRequest.tmuxSpec.terminal.attachCommand.includes("tmux -L pi-agents attach -t"));
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.mode, "terminal");
	}));

test("subagent-runner pi invocation and registration: manual subagent command parses label and timeout", () =>
	withTempAgentDir(async (agentDir) => {
		const commands: Record<string, any> = {};
		let capturedRequest: any;
		subagentRunner.default(
			{
				registerTool: () => {},
				registerCommand: (name: string, command: any) => {
					commands[name] = command;
				},
			} as any,
			async (request) => {
				capturedRequest = request;
				return { exitCode: 0, stdout: "ok", stderr: "", result: "ok" };
			},
		);

		await commands.subagent.handler("--label qa --timeout 600 check repo", {
			cwd: "/repo",
			ui: { notify: () => {}, setStatus: () => {} },
			agentDir,
		});
		for (let i = 0; i < 5 && !capturedRequest; i++) await Promise.resolve();
		assert.equal(capturedRequest.label, "qa");
		assert.equal(capturedRequest.timeoutSeconds, 600);
	}));

test("subagent-runner pi invocation and registration: manual terminal flag creates terminal workspace", () =>
	withTempAgentDir(async (agentDir) => {
		const notifications: string[] = [];
		const commands: Record<string, any> = {};
		let capturedRequest: any;
		subagentRunner.default(
			{
				registerTool: () => {},
				registerCommand: (name: string, command: any) => {
					commands[name] = command;
				},
			} as any,
			async (request) => {
				capturedRequest = request;
				return { exitCode: 0, stdout: "ok", stderr: "", result: "ok" };
			},
		);

		await commands.subagent.handler("--terminal check repo", {
			cwd: "/repo",
			ui: { notify: (message: string) => notifications.push(message), setStatus: () => {} },
			agentDir,
		});
		for (let i = 0; i < 5 && !capturedRequest; i++) await Promise.resolve();
		assert.ok(capturedRequest.tmuxSpec.terminal.attachCommand.includes("tmux -L pi-agents attach -t"));
		assert.equal(subagentRunner.loadSubagentRegistry(agentDir).runs[0]?.mode, "terminal");
		assert.ok(notifications[0]?.includes("Subagent manual started"));
	}));

test("subagent-runner pi invocation and registration: manual subagent command notifies immediately with compact output guidance", () =>
	withTempAgentDir(async (agentDir) => {
		const notifications: string[] = [];
		const commands: Record<string, any> = {};
		let release!: () => void;
		subagentRunner.default(
			{
				registerTool: () => {},
				registerCommand: (name: string, command: any) => {
					commands[name] = command;
				},
			} as any,
			async () => {
				await new Promise<void>((resolve) => {
					release = resolve;
				});
				return { exitCode: 0, stdout: "ok", stderr: "", result: "ok" };
			},
		);

		const promise = commands.subagent.handler("check repo", {
			cwd: "/repo",
			ui: {
				notify: (message: string) => notifications.push(message),
				setStatus: () => {},
			},
			agentDir,
		});
		await Promise.resolve();
		assert.ok(notifications[0]?.includes("Subagent manual started"));
		assert.ok(notifications[0]?.includes("Output: /subagents or subagent_result"));
		assert.ok(!notifications[0]?.includes("transcript.jsonl"));
		release();
		await promise;
		for (let i = 0; i < 5 && notifications.length < 2; i++) await Promise.resolve();
		assert.ok(notifications.at(-1)?.includes("Subagent manual completed"));
		assert.ok(notifications.at(-1)?.includes("Full output: /subagents or subagent_result"));
		assert.ok(!notifications.at(-1)?.includes("transcript.jsonl"));
	}));
