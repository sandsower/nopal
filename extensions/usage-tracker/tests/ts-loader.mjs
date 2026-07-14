/**
 * Test-only ESM resolve hook.
 *
 * Production sources in this extension use the pi idiom of relative
 * imports with a `.js` suffix even though no build step ever produces a
 * `.js` file (pi's own extension loader maps that suffix back to the
 * sibling `.ts` file). Plain `node --test` does not do that mapping, so
 * when a pure module under test imports another sibling module (e.g.
 * `usage-tracker-providers.ts` importing `./usage-tracker-formatting.js`),
 * Node's own resolver can't find a real `usage-tracker-formatting.js` and
 * fails the whole file.
 *
 * This hook only rewrites *relative* specifiers ending in `.js` to `.ts`,
 * and only as a fallback if the `.js` file doesn't actually resolve. It
 * never touches bare specifiers (npm packages, node: builtins), so it
 * can't mask a genuinely missing package.
 */
export async function resolve(specifier, context, nextResolve) {
	const isRelative = specifier.startsWith("./") || specifier.startsWith("../");
	if (isRelative && specifier.endsWith(".js")) {
		try {
			return await nextResolve(specifier.replace(/\.js$/, ".ts"), context);
		} catch {
			// Fall through to default resolution below (e.g. a real .js file).
		}
	}
	return nextResolve(specifier, context);
}
