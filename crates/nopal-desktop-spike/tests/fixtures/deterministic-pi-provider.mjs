import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";

const PROVIDER = "nopal-proof";
const MODEL = "deterministic";
const SECOND_MODEL = "deterministic-b";
const MODEL_PROMPT = "model switch proof";
const TOOL_PROMPT = "tool loop proof";
const TOOL_CALL_ID = "nopal-proof-read-cargo";
const SHELL_PROMPT = "shell activity proof";
const SHELL_CALL_ID = "nopal-proof-shell-printf";
const SLOW_FIFO_PROMPT = "slow FIFO first";
const SLOW_FIFO_DELAY_MS = 500;

function userText(message) {
	if (!message || message.role !== "user") return "";
	if (typeof message.content === "string") return message.content;
	return message.content
		.filter((part) => part.type === "text")
		.map((part) => part.text)
		.join("");
}

function latestPrompt(context) {
	for (let index = context.messages.length - 1; index >= 0; index -= 1) {
		const text = userText(context.messages[index]);
		if (text) return text;
	}
	return "missing prompt";
}

function completedProofTool(context, toolCallId, toolName) {
	return context.messages.some((message) =>
		message?.role === "toolResult"
		&& message.toolCallId === toolCallId
		&& message.toolName === toolName
		&& message.isError === false,
	);
}

function outputMessage(model, stopReason) {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 1,
			output: 1,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 2,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason,
		timestamp: Date.now(),
	};
}

function pushText(stream, output, text) {
	const contentIndex = output.content.length;
	output.content.push({ type: "text", text: "" });
	stream.push({ type: "text_start", contentIndex, partial: structuredClone(output) });
	output.content[contentIndex].text = text;
	stream.push({ type: "text_delta", contentIndex, delta: text, partial: structuredClone(output) });
	stream.push({ type: "text_end", contentIndex, content: text, partial: structuredClone(output) });
}

function pushToolCall(stream, output, toolCall) {
	const contentIndex = output.content.length;
	output.content.push({ ...toolCall, arguments: {} });
	stream.push({ type: "toolcall_start", contentIndex, partial: structuredClone(output) });
	const delta = JSON.stringify(toolCall.arguments);
	output.content[contentIndex].arguments = toolCall.arguments;
	stream.push({ type: "toolcall_delta", contentIndex, delta, partial: structuredClone(output) });
	stream.push({ type: "toolcall_end", contentIndex, toolCall, partial: structuredClone(output) });
}

function streamDeterministic(model, context) {
	const stream = createAssistantMessageEventStream();
	const prompt = latestPrompt(context);
	const complete = () => {
		const readTool = {
			type: "toolCall",
			id: TOOL_CALL_ID,
			name: "read",
			arguments: { path: "Cargo.toml", limit: 1 },
		};
		const shellTool = {
			type: "toolCall",
			id: SHELL_CALL_ID,
			name: "bash",
			arguments: { command: "printf nopal-shell-proof" },
		};
		const selectedTool = prompt === TOOL_PROMPT ? readTool : prompt === SHELL_PROMPT ? shellTool : undefined;
		const needsTool = selectedTool !== undefined
			&& !completedProofTool(context, selectedTool.id, selectedTool.name);
		const output = outputMessage(model, needsTool ? "toolUse" : "stop");
		stream.push({ type: "start", partial: structuredClone(output) });
		if (needsTool) {
			pushText(stream, output, `Nopal deterministic tool prelude: ${prompt}`);
			pushToolCall(stream, output, selectedTool);
		} else if (selectedTool !== undefined) {
			pushText(stream, output, `Nopal deterministic assistant after tool: ${prompt}`);
		} else if (prompt === MODEL_PROMPT) {
			pushText(stream, output, `Nopal deterministic model: ${model.id}`);
		} else {
			pushText(stream, output, `Nopal deterministic assistant: ${prompt}`);
		}
		stream.push({ type: "done", reason: output.stopReason, message: output });
		stream.end(output);
	};
	if (prompt === SLOW_FIFO_PROMPT) setTimeout(complete, SLOW_FIFO_DELAY_MS);
	else queueMicrotask(complete);
	return stream;
}

export default function deterministicProvider(pi) {
	pi.registerProvider(PROVIDER, {
		name: "Nopal deterministic proof provider",
		baseUrl: "http://127.0.0.1:1/not-used",
		apiKey: "deterministic-local-fixture",
		api: "nopal-proof-api",
		models: [MODEL, SECOND_MODEL].map((id) => ({
			id,
			name: id === MODEL ? "Nopal deterministic proof model" : "Nopal deterministic proof model B",
			reasoning: false,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 4096,
			maxTokens: 256,
		})),
		streamSimple: streamDeterministic,
	});
}
