// ---
// relationships:
//   validates: intent-driven-polyglot-release
// ---

"use strict";

const assert = require("node:assert/strict");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

test("npm pack contains only the public launcher contract", () => {
  const packageRoot = path.resolve(__dirname, "..");
  const metadata = require(path.join(packageRoot, "package.json"));
  assert.equal(metadata.name, "@wyrd-company/intentional");
  assert.equal(metadata.license, "Apache-2.0");
  assert.equal(metadata.repository.url, "git+https://github.com/wyrd-company/intentional.git");
  assert.equal(metadata.engines.node, ">=18");
  assert.deepEqual(metadata.bin, { intentional: "bin/intentional.js" });
  assert.deepEqual(metadata.publishConfig, {
    access: "public",
    registry: "https://registry.npmjs.org",
  });
  const result = spawnSync(
    "npm",
    ["pack", "--dry-run", "--json", "--ignore-scripts", "--userconfig=/dev/null"],
    { cwd: packageRoot, encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr);
  const [packed] = JSON.parse(result.stdout);
  assert.equal(packed.name, "@wyrd-company/intentional");
  assert.deepEqual(
    packed.files.map((file) => file.path).sort(),
    ["README.md", "bin/intentional.js", "install.js", "package.json"],
  );
});
