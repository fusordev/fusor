import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const manifestUrl = new URL("./manifest.json", import.meta.url);
const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));

function observeThrow(error) {
  return {
    kind: "throw",
    name: String(error?.name),
    message: String(error?.message),
  };
}

for (const testCase of manifest.cases) {
  let observation;
  try {
    observation = { kind: "string", value: String(Function(testCase.body)()) };
  } catch (error) {
    observation = observeThrow(error);
  }
  assert.deepStrictEqual(observation, testCase.expect, testCase.id);
}

console.log(
  `node weak-collections differential: ${manifest.cases.length}/${manifest.cases.length} cases match`,
);
