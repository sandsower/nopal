import { readFileSync } from "node:fs";
import { basename, dirname } from "node:path";
import { Key, matchesKey, truncateToWidth, visibleWidth } from "@earendil-works/pi-tui";
import type { SubagentRunRecord } from "./index.js";

export type RenderListOptions = {
	selectedIndex: number;
	width: number;
	now?: number;
	confirm?: SubagentUiConfirm;
	maxRows?: number;
};

export type RenderDetailOptions = {
	width: number;
	now?: number;
};

export type SubagentUiConfirm = { type: "kill" | "cleanup"; id: string } | { type: "dismiss-completed" };

export type SubagentUiState = {
	selectedIndex: number;
	detail: boolean;
	confirm?: SubagentUiConfirm;
};

export type SubagentUiAction =
	| { type: "dismiss"; id: string }
	| { type: "dismiss-completed" }
	| { type: "kill"; id: string }
	| { type: "cleanup"; id: string }
	| { type: "close" };

export function reduceSubagentUiState(
	state: SubagentUiState,
	input: string,
	runs: SubagentRunRecord[],
): { state: SubagentUiState; action?: SubagentUiAction } {
	const maxIndex = Math.max(0, runs.length - 1);
	const selectedIndex = Math.min(Math.max(0, state.selectedIndex), maxIndex);
	const current = runs[selectedIndex];
	const key = normalizeInput(input);
	if (state.confirm) {
		if (key === "y") {
			const action = state.confirm.type === "dismiss-completed"
				? { type: "dismiss-completed" as const }
				: { type: state.confirm.type, id: state.confirm.id } as SubagentUiAction;
			return { state: { ...state, selectedIndex, confirm: undefined }, action };
		}
		if (key === "n" || key === "escape" || key === "q") {
			return { state: { ...state, selectedIndex, confirm: undefined } };
		}
		return { state: { ...state, selectedIndex } };
	}
	if (key === "down" || key === "j") {
		return { state: { ...state, selectedIndex: Math.min(maxIndex, selectedIndex + 1) } };
	}
	if (key === "up" || key === "k") {
		return { state: { ...state, selectedIndex: Math.max(0, selectedIndex - 1) } };
	}
	if (key === "enter") {
		return { state: { ...state, selectedIndex, detail: runs.length > 0 ? !state.detail : false } };
	}
	if (key === "escape" || key === "q") {
		if (state.detail) return { state: { ...state, selectedIndex, detail: false } };
		return { state: { ...state, selectedIndex }, action: { type: "close" } };
	}
	if (key === "d" && current && current.status !== "running") {
		return { state: { ...state, selectedIndex }, action: { type: "dismiss", id: current.id } };
	}
	if (key === "shift+d" && runs.some((run) => run.status !== "running")) {
		return { state: { ...state, selectedIndex, confirm: { type: "dismiss-completed" } } };
	}
	if (key === "shift+k" && current && current.status === "running") {
		return { state: { ...state, selectedIndex, confirm: { type: "kill", id: current.id } } };
	}
	if (key === "c" && current?.terminal?.cleanupStatus === "kept") {
		return { state: { ...state, selectedIndex, confirm: { type: "cleanup", id: current.id } } };
	}
	return { state: { ...state, selectedIndex } };
}

export function renderSubagentListLines(runs: SubagentRunRecord[], options: RenderListOptions): string[] {
	const width = Math.max(1, options.width);
	const body = ["j/k ↑/↓ select · Enter detail · K kill · c cleanup · d dismiss · D dismiss completed · q close", ""];
	if (options.confirm) body.push(confirmText(options.confirm), "");
	if (runs.length === 0) return frameLines("Subagents", body.concat("No subagent runs found."), width);

	const maxRows = Math.max(1, Math.floor(options.maxRows ?? 12));
	const selectedIndex = Math.min(Math.max(0, options.selectedIndex), runs.length - 1);
	const start = Math.min(Math.max(0, selectedIndex - maxRows + 1), Math.max(0, runs.length - maxRows));
	const visibleRuns = runs.slice(start, start + maxRows);
	if (runs.length > maxRows) body.push(`showing ${start + 1}-${start + visibleRuns.length} of ${runs.length}`, "");

	for (const [offset, run] of visibleRuns.entries()) {
		const index = start + offset;
		const cursor = index === selectedIndex ? ">" : " ";
		const symbol = statusSymbol(run.status);
		const mode = run.mode === "terminal" ? "t" : "h";
		const duration = formatRunDuration(run, options.now ?? Date.now());
		const label = fitCell(run.label, 12);
		const status = fitCell(run.status, 7);
		const cleanup = run.terminal?.cleanupStatus === "kept" ? " cleanup" : "";
		body.push(`${cursor} ${symbol} ${mode} ${padCell(compactId(run.id), 14)} ${padCell(label, 12)} ${padCell(status, 7)} ${padCell(duration, 6)} ${run.promptPreview}${cleanup}`);
	}
	return frameLines("Subagents", body, width);
}

export function renderSubagentDetailLines(run: SubagentRunRecord | undefined, options: RenderDetailOptions): string[] {
	const width = Math.max(1, options.width);
	if (!run) return frameLines("Subagent detail", ["", "Run not found."], width);
	const contentWidth = Math.max(1, width - 4);
	const lines = [
		"q/Esc back · Enter list",
		"",
		`id: ${run.id}`,
		`label: ${run.label}`,
		`status: ${run.status}`,
		`mode: ${run.mode}`,
		`cwd: ${shortPath(run.cwd, Math.max(12, contentWidth - 5))}`,
		`started: ${run.startedAt}`,
		`duration: ${formatRunDuration(run, options.now ?? Date.now())}`,
	];
	if (run.completedAt) lines.push(`completed: ${run.completedAt}`);
	if (run.exitCode !== undefined) lines.push(`exit: ${run.exitCode}`);
	if (run.error) lines.push(`error: ${run.error}`);
	if (run.terminal?.attachCommand) lines.push(`attach: ${run.terminal.attachCommand}`);
	if (run.terminal?.cleanupStatus === "kept") lines.push("cleanup available: press c from the list view");
	lines.push(
		"",
		"Artifacts:",
		`prompt: ${shortPath(run.promptPath, Math.max(12, contentWidth - 8))}`,
		`result: ${shortPath(run.resultPath, Math.max(12, contentWidth - 8))}`,
		`transcript: ${shortPath(run.transcriptPath, Math.max(12, contentWidth - 12))}`,
		`stderr: ${shortPath(run.stderrPath, Math.max(12, contentWidth - 8))}`,
		`metadata: ${shortPath(run.metadataPath, Math.max(12, contentWidth - 10))}`,
	);
	const resultTail = tailFile(run.resultPath, 6, contentWidth);
	if (resultTail.length > 0) lines.push("", "Result tail:", ...resultTail);
	const transcriptTail = tailFile(run.transcriptPath, 8, contentWidth);
	if (transcriptTail.length > 0) lines.push("", "Transcript tail:", ...transcriptTail);
	const stderrTail = tailFile(run.stderrPath, 6, contentWidth);
	if (stderrTail.length > 0) lines.push("", "stderr tail:", ...stderrTail);
	const metadataTail = tailFile(run.metadataPath, 8, contentWidth);
	if (metadataTail.length > 0) lines.push("", "Metadata tail:", ...metadataTail);
	return frameLines("Subagent detail", lines, width);
}

export function renderRunningSubagentsWidget(runs: SubagentRunRecord[], options: { width?: number; now?: number } = {}): string[] | undefined {
	const running = runs.filter((run) => run.status === "running");
	if (running.length === 0) return undefined;
	const width = Math.max(1, options.width ?? 100);
	const lines = [`subagents: ${running.length} running`];
	for (const run of running.slice(0, 3)) {
		lines.push(fitLine(`● ${run.label} ${formatRunDuration(run, options.now ?? Date.now())} — ${run.promptPreview}`, width));
	}
	if (running.length > 3) lines.push(`… ${running.length - 3} more`);
	return lines.map((line) => fitLine(line, width));
}

function normalizeInput(input: string): string {
	if (matchesKey(input, Key.down)) return "down";
	if (matchesKey(input, Key.up)) return "up";
	if (matchesKey(input, Key.enter)) return "enter";
	if (matchesKey(input, Key.escape)) return "escape";
	if (input.length === 1) {
		if (input === "K") return "shift+k";
		if (input === "D") return "shift+d";
		return input.toLowerCase();
	}
	return input;
}

function confirmText(confirm: SubagentUiConfirm): string {
	if (confirm.type === "dismiss-completed") return "Dismiss all completed runs from the list? y/N";
	return `Confirm ${confirm.type} ${confirm.id}? y/N`;
}

function statusSymbol(status: SubagentRunRecord["status"]): string {
	if (status === "running") return "●";
	if (status === "success") return "○";
	if (status === "killed") return "×";
	return "!";
}

function compactId(id: string): string {
	return id.replace(/^\d{4}-\d{2}-\d{2}t/, "").replace(/-000z-/, "-").slice(0, 14);
}

function shortPath(path: string, width: number): string {
	if (visibleWidth(path) <= width) return path;
	const base = basename(path);
	const parent = basename(dirname(path));
	const compact = parent && parent !== "." ? `…/${parent}/${base}` : `…/${base}`;
	return fitLine(compact, width);
}

function formatRunDuration(run: SubagentRunRecord, now: number): string {
	const start = Date.parse(run.startedAt);
	const end = run.completedAt ? Date.parse(run.completedAt) : now;
	if (!Number.isFinite(start) || !Number.isFinite(end)) return "";
	const seconds = Math.max(0, Math.round((end - start) / 1000));
	return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m${seconds % 60}s`;
}

function tailFile(path: string, maxLines: number, width: number): string[] {
	try {
		return readFileSync(path, "utf-8")
			.trimEnd()
			.split("\n")
			.filter(Boolean)
			.slice(-maxLines)
			.map((line) => fitLine(line, Math.max(1, width)));
	} catch {
		return [];
	}
}

function frameLines(title: string, body: string[], width: number): string[] {
	if (width < 8) return [title, ...body].map((line) => fitLine(line, width));
	const innerWidth = Math.max(1, width - 4);
	const titleText = ` ${title} `;
	const topFill = Math.max(0, width - 2 - visibleWidth(titleText));
	const top = `┌${titleText}${"─".repeat(topFill)}┐`;
	const bottom = `└${"─".repeat(Math.max(0, width - 2))}┘`;
	const framed = body.map((line) => {
		const content = padCell(fitLine(line, innerWidth), innerWidth);
		return `│ ${content} │`;
	});
	return [top, ...framed, bottom].map((line) => fitLine(line, width));
}

function fitCell(value: string, width: number): string {
	return fitLine(value, width);
}

function padCell(value: string, width: number): string {
	const fitted = fitLine(value, width);
	const padding = Math.max(0, width - visibleWidth(fitted));
	return `${fitted}${" ".repeat(padding)}`;
}

function fitLine(value: string, width: number): string {
	return truncateToWidth(value, Math.max(0, width), "…");
}
