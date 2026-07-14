import { join } from "node:path";
import { buildPiSubprocessInvocationFromPromptFile } from "./subagent-pi-spawn.js";
import type { SubagentInvocation, SubagentTerminalMetadata, createSubagentArtifactPaths } from "./index.js";

const TMUX_SOCKET_NAME = "pi-agents";
const SESSION_PREFIX = "pi-agent-";
const MAX_SESSION_NAME_LENGTH = 48;

export type TmuxWorkspaceSpecInput = {
	id: string;
	cwd: string;
	prompt: string;
	paths: ReturnType<typeof createSubagentArtifactPaths>;
};

export type TmuxWorkspaceSpec = {
	terminal: SubagentTerminalMetadata;
	start: SubagentInvocation;
	exitCodePath: string;
};

export function createTmuxWorkspaceSpec(input: TmuxWorkspaceSpecInput): TmuxWorkspaceSpec {
	const sessionName = safeTmuxSessionName(input.id);
	const exitCodePath = join(input.paths.artifactDir, "exit-code.txt");
	const terminal: SubagentTerminalMetadata = {
		backend: "tmux",
		socketName: TMUX_SOCKET_NAME,
		sessionName,
		attachCommand: `tmux -L ${TMUX_SOCKET_NAME} attach -t ${sessionName}`,
		cleanupStatus: "active",
	};
	return {
		terminal,
		exitCodePath,
		start: {
			command: "tmux",
			args: ["-L", TMUX_SOCKET_NAME, "new-session", "-d", "-s", sessionName, buildWorkspaceShellCommand(input, exitCodePath)],
		},
	};
}

export function buildTmuxKillInvocation(sessionName: string): SubagentInvocation {
	return { command: "tmux", args: ["-L", TMUX_SOCKET_NAME, "kill-session", "-t", sessionName] };
}

function safeTmuxSessionName(id: string): string {
	const clean = id.toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "subagent";
	return `${SESSION_PREFIX}${clean}`.slice(0, MAX_SESSION_NAME_LENGTH);
}

function buildWorkspaceShellCommand(input: TmuxWorkspaceSpecInput, exitCodePath: string): string {
	const invocation = buildPiSubprocessInvocationFromPromptFile(input.paths.promptPath);
	const envPrefix = Object.entries(invocation.env)
		.filter((entry): entry is [string, string] => typeof entry[1] === "string")
		.map(([key, value]) => `${key}=${shellQuote(value)}`)
		.join(" ");
	const piCommand = [shellQuote(invocation.command), ...invocation.args.map(shellQuote)].join(" ");
	const command = [
		`cd ${shellQuote(input.cwd)}`,
		`${envPrefix ? `${envPrefix} ` : ""}${piCommand} > ${shellQuote(input.paths.transcriptPath)} 2> ${shellQuote(input.paths.stderrPath)}`,
		"code=$?",
		`printf '%s\\n' "$code" > ${shellQuote(exitCodePath)}`,
		`if [ "$code" -eq 0 ]; then exit 0; fi`,
		`echo ${shellQuote("Subagent failed. Inspect artifacts, then exit this shell to close the kept session.")}`,
		'exec "${SHELL:-/bin/sh}"',
	];
	return command.join("; ");
}

function shellQuote(value: string): string {
	return `'${value.replace(/'/g, `'"'"'`)}'`;
}
