export const NOPAL_SKILLS = [
	"babysit",
	"blueprint",
	"break-spec",
	"debug",
	"doctor",
	"envelope",
	"fresh-eyes",
	"handoff",
	"implement",
	"kickoff",
	"poke-holes",
	"pr-patrol",
	"ready-for-review",
	"retro",
	"review",
	"review-response",
	"rinse",
	"setup",
	"show-me",
	"spec",
	"verify",
	"walk-the-diff",
] as const;

export type NopalSkill = (typeof NOPAL_SKILLS)[number];

export const COMMAND_COLLISIONS: Partial<Record<NopalSkill, string>> = {
	"show-me": "show-me-skill",
};

export const BOUNDARY_CAPABLE_SKILLS = new Set<NopalSkill>([
	"break-spec",
	"blueprint",
	"envelope",
	"handoff",
	"implement",
	"kickoff",
	"ready-for-review",
	"review-response",
	"spec",
]);

export function commandNameForSkill(skill: NopalSkill): string {
	return COMMAND_COLLISIONS[skill] ?? skill;
}

export function skillPrompt(skill: NopalSkill, args: string): string {
	const trimmed = args.trim();
	return trimmed ? `/skill:${skill} ${trimmed}` : `/skill:${skill}`;
}

const SKILL_AUTOCOMPLETE_PREFIX = "skill:";

/**
 * Minimal shape of pi's `AutocompleteItem` (`@earendil-works/pi-tui`) needed for the
 * picker filter below. For slash-command suggestions, `value` and `label` both equal
 * the bare command name with no leading slash - verified against
 * `CombinedAutocompleteProvider.getSuggestions` in
 * `@earendil-works/pi-tui/dist/autocomplete.js`, so a skill's native picker entry has
 * `value === "skill:<name>"`.
 */
export type AutocompleteItemLike = {
	value: string;
};

/**
 * True when an autocomplete item is the native `/skill:<name>` entry for a skill that
 * Nopal already wraps with its own command (e.g. `skill:kickoff`, since `/kickoff`
 * exists). Skills not in `NOPAL_SKILLS` - a user's own skills, ambient or pinned in the
 * bundle - are left untouched so they keep their native `/skill:<name>` picker entry.
 */
export function isWrappedSkillAutocompleteValue(value: string): boolean {
	if (!value.startsWith(SKILL_AUTOCOMPLETE_PREFIX)) return false;
	return (NOPAL_SKILLS as readonly string[]).includes(value.slice(SKILL_AUTOCOMPLETE_PREFIX.length));
}

/**
 * Drop the native `/skill:<name>` picker entries duplicated by a Nopal wrapper
 * command, leaving every other item (built-ins, other extensions' commands, and any
 * skill Nopal doesn't wrap) untouched and in their original order.
 */
export function filterWrappedSkillAutocompleteItems<T extends AutocompleteItemLike>(items: readonly T[]): T[] {
	return items.filter((item) => !isWrappedSkillAutocompleteValue(item.value));
}
