#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import { pathToFileURL } from "node:url";
import Ajv2020 from "ajv/dist/2020.js";

const EVENT_FAMILIES = [
	"rondo.run.evidence_recorded",
	"rondo.run.status_changed",
	"rondo.service.status_changed",
];

function readJson(path) {
	return JSON.parse(fs.readFileSync(path, "utf8"));
}

function cursorOffset(cursor) {
	const match = /^rondo\.core\/v1:(\d+)$/u.exec(cursor);
	assert.notEqual(match, null, `invalid Core cursor: ${cursor}`);
	return Number.parseInt(match[1], 10);
}

export function compileSchema(schemaPath) {
	const ajv = new Ajv2020({ allErrors: true, strict: true });
	return ajv.compile(readJson(schemaPath));
}

export function validateDocuments(schemaPath, documentPaths) {
	const validate = compileSchema(schemaPath);

	for (const documentPath of documentPaths) {
		const valid = validate(readJson(documentPath));
		if (!valid) {
			const diagnostics = validate.errors
				.map((error) => `${error.instancePath || "/"} ${error.message}`)
				.join("\n");
			throw new Error(`${documentPath} does not conform to ${schemaPath}:\n${diagnostics}`);
		}
	}
}

export function assertFixtureReplay(archivedPath, resumePath) {
	const archived = readJson(archivedPath);
	const resumed = readJson(resumePath);

	assert.notEqual(archived.events.length, 0, "archived replay fixture must not be empty");
	assert.deepEqual(
		[...new Set(archived.events.map((event) => event.type))].sort(),
		EVENT_FAMILIES,
		"archived replay fixture must cover exactly the three Core event families",
	);
	assert.deepEqual(
		archived.events.map((event) => event.sequence),
		archived.events.map((_event, index) => index + 1),
		"archived replay fixture sequences must be contiguous",
	);
	assert.equal(
		cursorOffset(archived.next_event_cursor),
		archived.events.length,
		"archived replay cursor must equal the number of events consumed from zero",
	);
	assert.deepEqual(
		resumed.events,
		archived.events.slice(2),
		"resume fixture must be the exact tail after rondo.core/v1:2",
	);
	assert.equal(
		cursorOffset(resumed.next_event_cursor),
		2 + resumed.events.length,
		"resume fixture cursor must advance exactly by its returned tail length",
	);
	assert.equal(resumed.surface, archived.surface);
	assert.equal(resumed.repo_id, archived.repo_id);
	assert.equal(resumed.plot_id, archived.plot_id);
	assert.equal(resumed.run_id, archived.run_id);
	assert.equal(resumed.next_event_cursor, archived.next_event_cursor);
	assert.equal(resumed.has_more, archived.has_more);
	assert.equal(archived.has_more, false, "archived replay fixture must reach the live tail");
}

function usage() {
	console.error(
		"usage: validate-rondo-core-run-events.mjs documents <schema> <document>... | fixtures <schema> <archived> <resume>",
	);
}

export function main(args) {
	const [mode, schemaPath, ...documentPaths] = args;

	if (!schemaPath || documentPaths.length === 0) {
		usage();
		return 2;
	}

	if (mode === "documents") {
		validateDocuments(schemaPath, documentPaths);
		return 0;
	}

	if (mode === "fixtures" && documentPaths.length === 2) {
		validateDocuments(schemaPath, documentPaths);
		assertFixtureReplay(documentPaths[0], documentPaths[1]);
		return 0;
	}

	usage();
	return 2;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
	try {
		process.exitCode = main(process.argv.slice(2));
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exitCode = 1;
	}
}
