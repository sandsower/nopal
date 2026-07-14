import { spawn } from "node:child_process";
import { loadNopalModule } from "../../extensions/nopal/tests/setup.ts";

const [repoRoot, manifestPath, plotId, mode = "both", repoId, runId, eventCursor] = process.argv.slice(2);
const nopalBin = process.env.NOPAL_BIN;

if (!repoRoot || !manifestPath || !plotId || !nopalBin) {
	throw new Error("repo root, manifest path, Plot id, and NOPAL_BIN are required");
}

const extension = await loadNopalModule("../index.js");

function exec(command, args, options = {}) {
	if (command !== "nopal") {
		return Promise.reject(new Error(`unexpected executable requested by Nopal extension: ${command}`));
	}
	const env = { ...process.env };
	delete env.NOPAL_RONDO_CORE_URL;
	return new Promise((resolve, reject) => {
		const child = spawn(nopalBin, args, {
			cwd: options.cwd,
			env,
			signal: options.signal,
			timeout: options.timeout,
			stdio: ["ignore", "pipe", "pipe"],
		});
		const stdout = [];
		const stderr = [];
		child.stdout.on("data", (chunk) => stdout.push(chunk));
		child.stderr.on("data", (chunk) => stderr.push(chunk));
		child.once("error", reject);
		child.once("close", (code, signal) => {
			resolve({
				stdout: Buffer.concat(stdout).toString("utf8"),
				stderr: Buffer.concat(stderr).toString("utf8"),
				code: code ?? 1,
				killed: signal !== null,
			});
		});
	});
}

const tools = {};
const pi = {
	registerTool(tool) {
		tools[tool.name] = tool;
	},
	registerCommand() {},
	on() {},
	exec,
	sendUserMessage: async () => {},
	appendEntry() {},
};
extension.default(pi);

const context = { cwd: repoRoot, hasUI: false };
const startTool = tools.nopal_afk_start;
const resultTool = tools.nopal_afk_result;
if (!startTool || !resultTool) throw new Error("Nopal AFK tools were not registered");

let started;
let handle;
if (mode !== "result") {
	started = await startTool.execute(
		"conformance-start",
		{ manifestPath, plotId },
		undefined,
		undefined,
		context,
	);
	if (started.isError || !started.details?.handle) {
		throw new Error(`AFK start failed: ${JSON.stringify(started.details)}`);
	}
	handle = started.details.handle;
	if (mode === "start") {
		process.stdout.write(`${JSON.stringify({ start: started.details })}\n`);
	}
} else {
	if (!repoId || !runId || !eventCursor) {
		throw new Error("repo id, run id, and event cursor are required in result mode");
	}
	handle = { repo_id: repoId, plot_id: plotId, run_id: runId, event_cursor: eventCursor };
}

if (mode === "start") {
	process.exitCode = 0;
} else {
	const result = await resultTool.execute(
		"conformance-result",
		{
			repoId: handle.repo_id,
			plotId: handle.plot_id,
			runId: handle.run_id,
			eventCursor: handle.event_cursor,
			block: true,
			timeoutMs: 10_000,
			pollIntervalMs: 10,
		},
		undefined,
		undefined,
		context,
	);
	if (result.isError) {
		throw new Error(`AFK result failed: ${JSON.stringify(result.details)}`);
	}

	process.stdout.write(`${JSON.stringify({ start: started?.details, result: result.details })}\n`);
}
