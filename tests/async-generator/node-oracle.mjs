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
    const state = Function(testCase.body)();
    await Promise.resolve(state.done);
    observation = { kind: "string", value: state.result };
  } catch (error) {
    observation = observeThrow(error);
  }
  assert.deepStrictEqual(observation, testCase.expect, testCase.id);
}

console.log(
  `node async-generator differential: ${manifest.cases.length}/${manifest.cases.length} cases match`,
);
