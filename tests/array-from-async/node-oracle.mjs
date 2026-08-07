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

async function awaitCase(value, id) {
  let timer;
  try {
    await Promise.race([
      Promise.resolve(value),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`timed out while awaiting ${id}`)),
          5_000,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

assert.equal(
  typeof Array.fromAsync,
  "function",
  "the Node oracle must expose Array.fromAsync",
);

for (const testCase of manifest.cases) {
  let observation;
  try {
    const state = Function(testCase.body)();
    await awaitCase(state.done, testCase.id);
    observation = { kind: "string", value: state.result };
  } catch (error) {
    observation = observeThrow(error);
  }
  assert.deepStrictEqual(observation, testCase.expect, testCase.id);
}

console.log(
  `node Array.fromAsync differential: ${manifest.cases.length}/${manifest.cases.length} cases match`,
);
