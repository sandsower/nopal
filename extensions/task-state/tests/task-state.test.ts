import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import * as taskState from "../index.ts";

function withTempAgentDir(fn: (dir: string) => void) {
	const dir = mkdtempSync(join(tmpdir(), "task-state-test-"));
	try {
		fn(dir);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

// ---------------------------------------------------------------------------
// task-state scope and storage
// ---------------------------------------------------------------------------

test("task-state scope: builds deterministic slug from git root and branch", () => {
	const a = taskState.buildTaskScope({ cwd: "/repo/worktree", gitRoot: "/repo", branch: "feature/task-state" });
	const b = taskState.buildTaskScope({ cwd: "/repo/other", gitRoot: "/repo", branch: "feature/task-state" });
	const c = taskState.buildTaskScope({ cwd: "/repo/worktree", gitRoot: "/repo", branch: "main" });

	assert.equal(a.slug, b.slug);
	assert.notEqual(a.slug, c.slug);
	assert.equal(a.branch, "feature/task-state");
});

test("task-state scope: creates empty state for a scope", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	const state = taskState.createEmptyTaskState(scope, 1000);

	assert.deepEqual(state.scope, scope);
	assert.equal(state.updatedAt, 1000);
	assert.deepEqual(state.tasks, []);
	assert.deepEqual(state.checkpoints, []);
});

test("task-state storage: persists and reloads a task", () =>
	withTempAgentDir((agentDir) => {
		const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
		let state = taskState.createEmptyTaskState(scope, 1000);
		state = taskState.createTask(state, { title: "Write tests", notes: "TDD first" }, 1100);
		taskState.saveTaskState(agentDir, state);

		const loaded = taskState.loadTaskState(agentDir, scope, 1200);
		assert.equal(loaded.tasks.length, 1);
		assert.equal(loaded.tasks[0]?.id, "t1");
		assert.equal(loaded.tasks[0]?.title, "Write tests");
		assert.equal(loaded.tasks[0]?.status, "todo");
		assert.equal(loaded.tasks[0]?.notes, "TDD first");
		assert.ok(taskState.taskStatePath(agentDir, scope).includes(scope.slug));
	}));

// ---------------------------------------------------------------------------
// task-state mutations
// ---------------------------------------------------------------------------

test("task-state mutations: updates task fields", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	let state = taskState.createTask(taskState.createEmptyTaskState(scope, 1000), { title: "Old" }, 1100);
	state = taskState.updateTask(state, "t1", { title: "New", status: "in_progress", notes: "Working" }, 1200);

	assert.equal(state.tasks[0]?.id, "t1");
	assert.equal(state.tasks[0]?.title, "New");
	assert.equal(state.tasks[0]?.status, "in_progress");
	assert.equal(state.tasks[0]?.notes, "Working");
	assert.equal(state.tasks[0]?.updatedAt, 1200);
});

test("task-state mutations: completes task", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	let state = taskState.createTask(taskState.createEmptyTaskState(scope, 1000), { title: "Do it" }, 1100);
	state = taskState.completeTask(state, "t1", 1200);
	assert.equal(state.tasks[0]?.status, "done");
});

test("task-state mutations: throws clear error for unknown task", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	const state = taskState.createEmptyTaskState(scope, 1000);
	assert.throws(() => taskState.updateTask(state, "missing", { status: "done" }, 1200), /Task missing not found/);
});

test("task-state mutations: lists active tasks before done tasks", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	let state = taskState.createEmptyTaskState(scope, 1000);
	state = taskState.createTask(state, { title: "Done" }, 1100);
	state = taskState.completeTask(state, "t1", 1200);
	state = taskState.createTask(state, { title: "Blocked", status: "blocked" }, 1300);
	state = taskState.createTask(state, { title: "Todo" }, 1400);

	assert.deepEqual(
		taskState.listTasks(state).map((task) => task.title),
		["Todo", "Blocked", "Done"],
	);
});

// ---------------------------------------------------------------------------
// task-state checkpoints and formatting
// ---------------------------------------------------------------------------

test("task-state checkpoints: adds checkpoint and caps old entries", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "main" });
	let state = taskState.createEmptyTaskState(scope, 1000);
	for (let i = 1; i <= 7; i++) {
		state = taskState.addCheckpoint(state, `checkpoint ${i}`, 1000 + i, 5);
	}

	assert.equal(state.checkpoints.length, 5);
	assert.equal(state.checkpoints[0]?.text, "checkpoint 3");
	assert.equal(state.checkpoints[4]?.text, "checkpoint 7");
});

test("task-state formatting: formats task state for display", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "feature" });
	let state = taskState.createEmptyTaskState(scope, 1000);
	state = taskState.createTask(state, { title: "Active", status: "in_progress" }, 1100);
	state = taskState.createTask(state, { title: "Blocked", status: "blocked" }, 1200);
	state = taskState.addCheckpoint(state, "Need to wire command", 1300);

	const output = taskState.formatTaskState(state);
	assert.ok(output.includes("branch: feature"));
	assert.ok(output.includes("Active"));
	assert.ok(output.includes("Blocked"));
	assert.ok(output.includes("Need to wire command"));
});

test("task-state formatting: formats compact status line", () => {
	const scope = taskState.buildTaskScope({ cwd: "/repo", branch: "feature" });
	let state = taskState.createEmptyTaskState(scope, 1000);
	assert.equal(taskState.formatTaskStatusLine(state), undefined);
	state = taskState.createTask(state, { title: "Active", status: "in_progress" }, 1100);
	state = taskState.createTask(state, { title: "Blocked", status: "blocked" }, 1200);
	assert.equal(taskState.formatTaskStatusLine(state), "tasks: 2 active · 1 blocked");
});
