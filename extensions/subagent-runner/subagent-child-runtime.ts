import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const CHILD_SUBAGENT_BOUNDARY_INSTRUCTIONS = [
	"You are a focused child subagent running in an isolated pi context.",
	"You are not the parent orchestrator. Do not propose, launch, or coordinate subagents.",
	"Complete only the delegated task with the tools available to you.",
	"Default policy: read-only. Do not edit files, run destructive commands, or mutate external systems unless the delegated task explicitly says mutations are allowed.",
	"If edits are explicitly allowed, use the actual edit/write tools. Do not print pseudo-tool calls, patches, or tool syntax as text.",
	"Return concise findings for the parent session.",
].join("\n");

export const PARENT_SUBAGENT_TOOL_NAMES = new Set([
	"subagent_start",
	"subagent_result",
	"subagent_list",
	"subagent_kill",
	"subagent_run",
]);

const PARENT_ONLY_CUSTOM_MESSAGE_TYPES = new Set([
	"subagent-orchestration-instructions",
	"subagent-notify",
	"subagent_control_notice",
	"subagent-control",
	"subagent-control-notice",
]);

export function rewriteChildSystemPrompt(systemPrompt: string): string {
	return systemPrompt.includes(CHILD_SUBAGENT_BOUNDARY_INSTRUCTIONS)
		? systemPrompt
		: `${CHILD_SUBAGENT_BOUNDARY_INSTRUCTIONS}\n\n${systemPrompt}`;
}

function isKnownSubagentToolName(value: unknown): value is string {
	return typeof value === "string" && PARENT_SUBAGENT_TOOL_NAMES.has(value);
}

function isParentOnlySubagentMessage(message: unknown): boolean {
	if (!message || typeof message !== "object") return false;
	const record = message as { role?: unknown; customType?: unknown; toolName?: unknown; name?: unknown };
	if (record.role === "custom" && typeof record.customType === "string" && PARENT_ONLY_CUSTOM_MESSAGE_TYPES.has(record.customType)) return true;
	if ((record.role === "toolResult" || record.role === "tool_result") && isKnownSubagentToolName(record.toolName ?? record.name)) return true;
	return false;
}

function isSubagentToolCallBlock(block: unknown): boolean {
	if (!block || typeof block !== "object") return false;
	const record = block as { type?: unknown; name?: unknown; toolName?: unknown };
	return (record.type === "toolCall" || record.type === "tool_call") && isKnownSubagentToolName(record.name ?? record.toolName);
}

function stripAssistantSubagentToolCallBlocks(message: unknown): unknown | undefined {
	if (!message || typeof message !== "object") return message;
	const record = message as { role?: unknown; content?: unknown };
	if (record.role !== "assistant" || !Array.isArray(record.content)) return message;
	const filteredContent = record.content.filter((block) => !isSubagentToolCallBlock(block));
	if (filteredContent.length === record.content.length) return message;
	if (filteredContent.length === 0) return undefined;
	return { ...record, content: filteredContent };
}

export function stripParentSubagentArtifacts(messages: unknown[]): unknown[] {
	let changed = false;
	const filtered: unknown[] = [];
	for (const message of messages) {
		if (isParentOnlySubagentMessage(message)) {
			changed = true;
			continue;
		}
		const stripped = stripAssistantSubagentToolCallBlocks(message);
		if (stripped === undefined) {
			changed = true;
			continue;
		}
		if (stripped !== message) changed = true;
		filtered.push(stripped);
	}
	return changed ? filtered : messages;
}

type ActiveToolsApi = Pick<ExtensionAPI, "getActiveTools" | "setActiveTools">;

type NamedTool = string | { name?: unknown };

function hasActiveToolsApi(pi: ExtensionAPI): pi is ExtensionAPI & ActiveToolsApi {
	const candidate = pi as Partial<ActiveToolsApi>;
	return typeof candidate.getActiveTools === "function" && typeof candidate.setActiveTools === "function";
}

function getActiveToolName(tool: NamedTool): string | undefined {
	return typeof tool === "string" ? tool : typeof tool.name === "string" ? tool.name : undefined;
}

export function activeToolNamesWithoutParentSubagents(tools: NamedTool[]): string[] {
	return tools
		.map(getActiveToolName)
		.filter((name): name is string => typeof name === "string" && !PARENT_SUBAGENT_TOOL_NAMES.has(name));
}

export function disableParentSubagentTools(pi: ExtensionAPI): boolean {
	if (!hasActiveToolsApi(pi)) return false;
	const activeTools = pi.getActiveTools() as unknown as NamedTool[];
	const nextToolNames = activeToolNamesWithoutParentSubagents(activeTools);
	if (nextToolNames.length === activeTools.length) return false;
	pi.setActiveTools(nextToolNames);
	return true;
}

export default function registerSubagentChildRuntime(pi: ExtensionAPI): void {
	const disableParentTools = () => {
		disableParentSubagentTools(pi);
	};

	pi.on("session_start", disableParentTools);
	pi.on("resources_discover", disableParentTools);
	pi.on("input", () => {
		disableParentTools();
		return { action: "continue" };
	});

	pi.on("context", (event: { messages?: unknown[] }) => {
		if (!Array.isArray(event.messages)) return undefined;
		const messages = stripParentSubagentArtifacts(event.messages);
		if (messages === event.messages) return undefined;
		return { messages };
	});

	pi.on("before_agent_start", (event: { systemPrompt?: string }) => {
		disableParentTools();
		if (typeof event.systemPrompt !== "string") return undefined;
		const systemPrompt = rewriteChildSystemPrompt(event.systemPrompt);
		if (systemPrompt === event.systemPrompt) return undefined;
		return { systemPrompt };
	});
}
