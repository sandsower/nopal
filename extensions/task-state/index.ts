import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export type TaskStatus = "todo" | "in_progress" | "blocked" | "done";

export type TaskScope = {
	cwd: string;
	gitRoot?: string;
	branch?: string;
	slug: string;
};

export type TaskItem = {
	id: string;
	title: string;
	status: TaskStatus;
	notes?: string;
	createdAt: number;
	updatedAt: number;
};

export type Checkpoint = {
	text: string;
	createdAt: number;
};

export type TaskState = {
	scope: TaskScope;
	updatedAt: number;
	tasks: TaskItem[];
	checkpoints: Checkpoint[];
};

export type BuildTaskScopeInput = {
	cwd: string;
	gitRoot?: string;
	branch?: string;
};

export type CreateTaskInput = {
	title: string;
	status?: TaskStatus;
	notes?: string;
};

export type UpdateTaskInput = {
	title?: string;
	status?: TaskStatus;
	notes?: string;
};

const TASK_STATE_DIR = "task-state";
const CHECKPOINT_LIMIT = 20;
const STATUS_VALUES = ["todo", "in_progress", "blocked", "done"] as const;

const TaskStatusSchema = Type.Union(STATUS_VALUES.map((value) => Type.Literal(value)));

export function buildTaskScope(input: BuildTaskScopeInput): TaskScope {
	const identity = `${input.gitRoot || input.cwd}#${input.branch || "no-branch"}`;
	const digest = createHash("sha256").update(identity).digest("hex").slice(0, 16);
	const label = sanitizeSlug(`${basenameForSlug(input.gitRoot || input.cwd)}-${input.branch || "cwd"}`);
	return {
		cwd: input.cwd,
		gitRoot: input.gitRoot,
		branch: input.branch,
		slug: `${label}-${digest}`,
	};
}

export function createEmptyTaskState(scope: TaskScope, now = Date.now()): TaskState {
	return { scope, updatedAt: now, tasks: [], checkpoints: [] };
}

export function taskStatePath(agentDir: string, scope: TaskScope): string {
	return join(agentDir, TASK_STATE_DIR, `${scope.slug}.json`);
}

export function loadTaskState(agentDir: string, scope: TaskScope, now = Date.now()): TaskState {
	const path = taskStatePath(agentDir, scope);
	if (!existsSync(path)) {
		return createEmptyTaskState(scope, now);
	}

	try {
		const parsed = JSON.parse(readFileSync(path, "utf-8")) as Partial<TaskState>;
		return normalizeTaskState(parsed, scope, now);
	} catch {
		return createEmptyTaskState(scope, now);
	}
}

export function saveTaskState(agentDir: string, state: TaskState): void {
	const path = taskStatePath(agentDir, state.scope);
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, `${JSON.stringify(state, null, 2)}\n`, "utf-8");
}

export function createTask(state: TaskState, input: CreateTaskInput, now = Date.now()): TaskState {
	const title = input.title.trim();
	if (!title) throw new Error("Task title is required");
	const nextTask: TaskItem = {
		id: nextTaskId(state),
		title,
		status: input.status ?? "todo",
		notes: cleanOptional(input.notes),
		createdAt: now,
		updatedAt: now,
	};
	return { ...state, updatedAt: now, tasks: [...state.tasks, nextTask] };
}

export function updateTask(state: TaskState, id: string, input: UpdateTaskInput, now = Date.now()): TaskState {
	let found = false;
	const tasks = state.tasks.map((task) => {
		if (task.id !== id) return task;
		found = true;
		return {
			...task,
			title: input.title !== undefined ? requireNonEmpty(input.title, "Task title is required") : task.title,
			status: input.status ?? task.status,
			notes: input.notes !== undefined ? cleanOptional(input.notes) : task.notes,
			updatedAt: now,
		};
	});
	if (!found) throw new Error(`Task ${id} not found`);
	return { ...state, updatedAt: now, tasks };
}

export function completeTask(state: TaskState, id: string, now = Date.now()): TaskState {
	return updateTask(state, id, { status: "done" }, now);
}

export function listTasks(state: TaskState): TaskItem[] {
	const rank: Record<TaskStatus, number> = { in_progress: 0, todo: 1, blocked: 2, done: 3 };
	return [...state.tasks].sort((a, b) => {
		const statusDiff = rank[a.status] - rank[b.status];
		if (statusDiff !== 0) return statusDiff;
		return b.updatedAt - a.updatedAt;
	});
}

export function addCheckpoint(state: TaskState, text: string, now = Date.now(), limit = CHECKPOINT_LIMIT): TaskState {
	const cleanText = text.trim();
	if (!cleanText) throw new Error("Checkpoint text is required");
	const checkpoints = [...state.checkpoints, { text: cleanText, createdAt: now }].slice(-limit);
	return { ...state, updatedAt: now, checkpoints };
}

export function formatTaskState(state: TaskState): string {
	const lines: string[] = [];
	lines.push("Task state");
	lines.push(`scope: ${state.scope.gitRoot ?? state.scope.cwd}`);
	if (state.scope.branch) lines.push(`branch: ${state.scope.branch}`);
	lines.push("");

	const tasks = listTasks(state);
	if (tasks.length === 0) {
		lines.push("No tasks recorded for this worktree.");
	} else {
		for (const status of STATUS_VALUES) {
			const group = tasks.filter((task) => task.status === status);
			if (group.length === 0) continue;
			lines.push(`${statusLabel(status)}:`);
			for (const task of group) {
				lines.push(`- ${task.id} ${task.title}${task.notes ? ` — ${task.notes}` : ""}`);
			}
			lines.push("");
		}
	}

	const latest = state.checkpoints.at(-1);
	if (latest) {
		lines.push("Latest checkpoint:");
		lines.push(latest.text);
	}

	return lines.join("\n").trimEnd();
}

export function formatTaskStatusLine(state: TaskState): string | undefined {
	const active = state.tasks.filter((task) => task.status !== "done").length;
	if (active === 0) return undefined;
	const blocked = state.tasks.filter((task) => task.status === "blocked").length;
	return `tasks: ${active} active${blocked > 0 ? ` · ${blocked} blocked` : ""}`;
}

export default function taskStateExtension(pi: ExtensionAPI) {
	let activeCtx: ExtensionContext | null = null;

	function currentScope(): TaskScope {
		return detectCurrentScope(process.cwd());
	}

	function loadCurrentState(): TaskState {
		return loadTaskState(getAgentDir(), currentScope());
	}

	function saveCurrentState(state: TaskState): void {
		saveTaskState(getAgentDir(), state);
		updateStatus(state);
	}

	function updateStatus(state = loadCurrentState()): void {
		const line = formatTaskStatusLine(state);
		activeCtx?.ui.setStatus("task-state", line);
	}

	pi.on("session_start", async (_event, ctx) => {
		activeCtx = ctx;
		updateStatus();
	});

	pi.on("session_shutdown", async () => {
		activeCtx?.ui.setStatus("task-state", undefined);
		activeCtx = null;
	});

	pi.registerCommand("tasks", {
		description: "Show worktree-scoped task state",
		handler: async (_args, ctx) => {
			activeCtx = ctx;
			const state = loadCurrentState();
			updateStatus(state);
			ctx.ui.notify(formatTaskState(state), "info");
		},
	});

	pi.registerTool({
		name: "task_state_list",
		label: "Task State List",
		description: "List durable worktree-scoped tasks and recent checkpoints. This is state only, not a continue-work workflow.",
		promptSnippet: "List durable worktree-scoped tasks and checkpoints",
		parameters: Type.Object({}),
		async execute() {
			const state = loadCurrentState();
			return textResult(formatTaskState(state), { state });
		},
	});

	pi.registerTool({
		name: "task_state_create",
		label: "Task State Create",
		description: "Create a durable worktree-scoped task for tracking current work.",
		promptSnippet: "Create a durable worktree-scoped task",
		parameters: Type.Object({
			title: Type.String({ description: "Short task title" }),
			status: Type.Optional(TaskStatusSchema),
			notes: Type.Optional(Type.String({ description: "Optional task notes" })),
		}),
		async execute(_toolCallId, params) {
			const state = createTask(loadCurrentState(), params);
			saveCurrentState(state);
			const task = state.tasks.at(-1);
			return textResult(`Created ${task?.id}: ${task?.title}`, { task, state });
		},
	});

	pi.registerTool({
		name: "task_state_update",
		label: "Task State Update",
		description: "Update a durable worktree-scoped task by id.",
		promptSnippet: "Update a durable worktree-scoped task",
		parameters: Type.Object({
			id: Type.String({ description: "Task id, e.g. t1" }),
			title: Type.Optional(Type.String({ description: "New task title" })),
			status: Type.Optional(TaskStatusSchema),
			notes: Type.Optional(Type.String({ description: "New task notes" })),
		}),
		async execute(_toolCallId, params) {
			try {
				const state = updateTask(loadCurrentState(), params.id, params);
				saveCurrentState(state);
				return textResult(`Updated ${params.id}`, { state });
			} catch (error) {
				return textResult(error instanceof Error ? error.message : String(error), { error: String(error) }, true);
			}
		},
	});

	pi.registerTool({
		name: "task_state_checkpoint",
		label: "Task State Checkpoint",
		description: "Save a durable worktree-scoped checkpoint note for current work.",
		promptSnippet: "Save a durable worktree-scoped checkpoint note",
		parameters: Type.Object({
			text: Type.String({ description: "Checkpoint text" }),
		}),
		async execute(_toolCallId, params) {
			const state = addCheckpoint(loadCurrentState(), params.text);
			saveCurrentState(state);
			return textResult("Checkpoint saved", { checkpoint: state.checkpoints.at(-1), state });
		},
	});
}

function detectCurrentScope(cwd: string): TaskScope {
	return buildTaskScope({ cwd, gitRoot: gitRoot(cwd), branch: gitBranch(cwd) });
}

function gitRoot(cwd: string): string | undefined {
	try {
		return execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd, encoding: "utf-8", stdio: ["ignore", "pipe", "ignore"] }).trim() || undefined;
	} catch {
		return undefined;
	}
}

function gitBranch(cwd: string): string | undefined {
	try {
		return execFileSync("git", ["branch", "--show-current"], { cwd, encoding: "utf-8", stdio: ["ignore", "pipe", "ignore"] }).trim() || undefined;
	} catch {
		return undefined;
	}
}

function normalizeTaskState(input: Partial<TaskState>, scope: TaskScope, now: number): TaskState {
	return {
		scope,
		updatedAt: typeof input.updatedAt === "number" ? input.updatedAt : now,
		tasks: Array.isArray(input.tasks) ? input.tasks.filter(isTaskItem) : [],
		checkpoints: Array.isArray(input.checkpoints) ? input.checkpoints.filter(isCheckpoint) : [],
	};
}

function isTaskItem(value: unknown): value is TaskItem {
	if (!value || typeof value !== "object") return false;
	const task = value as Partial<TaskItem>;
	return typeof task.id === "string" && typeof task.title === "string" && isTaskStatus(task.status) && typeof task.createdAt === "number" && typeof task.updatedAt === "number";
}

function isCheckpoint(value: unknown): value is Checkpoint {
	if (!value || typeof value !== "object") return false;
	const checkpoint = value as Partial<Checkpoint>;
	return typeof checkpoint.text === "string" && typeof checkpoint.createdAt === "number";
}

function isTaskStatus(value: unknown): value is TaskStatus {
	return typeof value === "string" && (STATUS_VALUES as readonly string[]).includes(value);
}

function nextTaskId(state: TaskState): string {
	let max = 0;
	for (const task of state.tasks) {
		const match = /^t(\d+)$/.exec(task.id);
		if (match) max = Math.max(max, Number(match[1]));
	}
	return `t${max + 1}`;
}

function textResult(text: string, details: Record<string, unknown>, isError = false) {
	return { content: [{ type: "text" as const, text }], details, isError };
}

function requireNonEmpty(value: string, message: string): string {
	const clean = value.trim();
	if (!clean) throw new Error(message);
	return clean;
}

function cleanOptional(value: string | undefined): string | undefined {
	const clean = value?.trim();
	return clean ? clean : undefined;
}

function statusLabel(status: TaskStatus): string {
	switch (status) {
		case "todo":
			return "Todo";
		case "in_progress":
			return "In progress";
		case "blocked":
			return "Blocked";
		case "done":
			return "Done";
	}
}

function basenameForSlug(path: string): string {
	const normalized = path.replaceAll("\\", "/").replace(/\/+$/, "");
	return normalized.split("/").pop() || "cwd";
}

function sanitizeSlug(value: string): string {
	return value.toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 80) || "task-state";
}
