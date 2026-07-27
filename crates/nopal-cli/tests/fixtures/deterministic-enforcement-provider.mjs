import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";

const PROVIDER = "nopal-enforcement-proof";
const MODEL = "deterministic";
const PROMPT = "enforcement walking skeleton proof";
const STEPS = [
	["push-initial", "git push origin HEAD:refs/heads/main"],
	["commit-change", "printf changed\\n >> source.txt && git add source.txt && git commit -m changed"],
	["push-stale", "git push origin HEAD:refs/heads/main"],
	["force-denied", "git push --force origin HEAD:refs/heads/main"],
];

function userText(message) {
	if (!message || message.role !== "user") return "";
	if (typeof message.content === "string") return message.content;
	return message.content.filter((part) => part.type === "text").map((part) => part.text).join("");
}

function latestPrompt(context) {
	for (let index = context.messages.length - 1; index >= 0; index -= 1) {
		const text = userText(context.messages[index]);
		if (text) return text;
	}
	return "";
}

function completedSteps(context) {
	return STEPS.filter(([id]) => context.messages.some((message) => message?.role === "toolResult" && message.toolCallId === id)).length;
}

function outputMessage(model, stopReason) {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason,
		timestamp: Date.now(),
	};
}

function stream(model, context) {
	const events = createAssistantMessageEventStream();
	queueMicrotask(() => {
		const prompt = latestPrompt(context);
		const step = prompt === PROMPT ? completedSteps(context) : STEPS.length;
		const needsTool = step < STEPS.length;
		const output = outputMessage(model, needsTool ? "toolUse" : "stop");
		events.push({ type: "start", partial: structuredClone(output) });
		if (needsTool) {
			const [id, command] = STEPS[step];
			const contentIndex = 0;
			const toolCall = { type: "toolCall", id, name: "bash", arguments: { command } };
			output.content.push({ ...toolCall, arguments: {} });
			events.push({ type: "toolcall_start", contentIndex, partial: structuredClone(output) });
			output.content[contentIndex].arguments = toolCall.arguments;
			events.push({ type: "toolcall_delta", contentIndex, delta: JSON.stringify(toolCall.arguments), partial: structuredClone(output) });
			events.push({ type: "toolcall_end", contentIndex, toolCall, partial: structuredClone(output) });
		} else {
			const contentIndex = 0;
			const text = "enforcement proof complete";
			output.content.push({ type: "text", text });
			events.push({ type: "text_start", contentIndex, partial: structuredClone(output) });
			events.push({ type: "text_delta", contentIndex, delta: text, partial: structuredClone(output) });
			events.push({ type: "text_end", contentIndex, content: text, partial: structuredClone(output) });
		}
		events.push({ type: "done", reason: output.stopReason, message: output });
		events.end(output);
	});
	return events;
}

export default function deterministicEnforcementProvider(pi) {
	pi.registerProvider(PROVIDER, {
		name: "Nopal enforcement proof provider",
		baseUrl: "http://127.0.0.1:1/not-used",
		apiKey: "deterministic-local-fixture",
		api: "nopal-enforcement-proof-api",
		models: [{
			id: MODEL,
			name: "Nopal deterministic enforcement proof",
			reasoning: false,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 4096,
			maxTokens: 256,
		}],
		streamSimple: stream,
	});
}
