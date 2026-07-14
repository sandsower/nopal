import assert from "node:assert/strict";
import { test } from "node:test";
import {
	BOUNDARY_CAPABLE_SKILLS,
	COMMAND_COLLISIONS,
	NOPAL_SKILLS,
	commandNameForSkill,
	filterWrappedSkillAutocompleteItems,
	isWrappedSkillAutocompleteValue,
	skillPrompt,
} from "../skill-commands.ts";

test("NOPAL_SKILLS: has no duplicate entries", () => {
	assert.equal(new Set(NOPAL_SKILLS).size, NOPAL_SKILLS.length);
});

test("commandNameForSkill: show-me remaps to show-me-skill to avoid a pi builtin collision", () => {
	assert.equal(commandNameForSkill("show-me"), "show-me-skill");
	assert.equal(COMMAND_COLLISIONS["show-me"], "show-me-skill");
});

test("commandNameForSkill: unmapped skills pass through unchanged", () => {
	for (const skill of NOPAL_SKILLS) {
		if (skill === "show-me") continue;
		assert.equal(commandNameForSkill(skill), skill);
	}
});

test("skillPrompt: no args produces a bare /skill:<name> prompt", () => {
	assert.equal(skillPrompt("kickoff", ""), "/skill:kickoff");
	assert.equal(skillPrompt("kickoff", "   "), "/skill:kickoff");
});

test("skillPrompt: args are trimmed and appended", () => {
	assert.equal(skillPrompt("babysit", "  --tokens 250k  "), "/skill:babysit --tokens 250k");
});

test("BOUNDARY_CAPABLE_SKILLS: only contains skills present in NOPAL_SKILLS", () => {
	for (const skill of BOUNDARY_CAPABLE_SKILLS) {
		assert.ok((NOPAL_SKILLS as readonly string[]).includes(skill), `${skill} must be a registered skill`);
	}
});

test("isWrappedSkillAutocompleteValue: true for a skill: entry backed by a NOPAL_SKILLS wrapper", () => {
	assert.equal(isWrappedSkillAutocompleteValue("skill:kickoff"), true);
	assert.equal(isWrappedSkillAutocompleteValue("skill:show-me"), true);
});

test("isWrappedSkillAutocompleteValue: false for a skill: entry with no Nopal wrapper (a user's own skill)", () => {
	assert.equal(isWrappedSkillAutocompleteValue("skill:my-custom-skill"), false);
});

test("isWrappedSkillAutocompleteValue: false for non-skill values, even if they share a wrapped skill's name", () => {
	assert.equal(isWrappedSkillAutocompleteValue("kickoff"), false);
	assert.equal(isWrappedSkillAutocompleteValue("skillful"), false);
	assert.equal(isWrappedSkillAutocompleteValue(""), false);
});

test("filterWrappedSkillAutocompleteItems: drops the native entry for a wrapped beislid skill", () => {
	const items = [{ value: "skill:kickoff", label: "skill:kickoff" }];
	assert.deepEqual(filterWrappedSkillAutocompleteItems(items), []);
});

test("filterWrappedSkillAutocompleteItems: keeps a foreign (non-NOPAL_SKILLS) skill's native entry", () => {
	const items = [{ value: "skill:my-custom-skill", label: "skill:my-custom-skill", description: "does a thing" }];
	assert.deepEqual(filterWrappedSkillAutocompleteItems(items), items);
});

test("filterWrappedSkillAutocompleteItems: keeps non-skill entries (built-ins, extension commands, file paths) untouched", () => {
	const items = [
		{ value: "model", label: "model" },
		{ value: "kickoff", label: "kickoff" }, // the Nopal wrapper command itself, not a skill: entry
		{ value: "show-me", label: "show-me" },
	];
	assert.deepEqual(filterWrappedSkillAutocompleteItems(items), items);
});

test("filterWrappedSkillAutocompleteItems: mixed list drops only the wrapped skill entries and preserves order", () => {
	const items = [
		{ value: "model", label: "model" },
		{ value: "skill:kickoff", label: "skill:kickoff" },
		{ value: "skill:my-custom-skill", label: "skill:my-custom-skill" },
		{ value: "skill:show-me", label: "skill:show-me" },
		{ value: "show-me", label: "show-me" },
	];
	assert.deepEqual(filterWrappedSkillAutocompleteItems(items), [
		{ value: "model", label: "model" },
		{ value: "skill:my-custom-skill", label: "skill:my-custom-skill" },
		{ value: "show-me", label: "show-me" },
	]);
});
