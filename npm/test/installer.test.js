// ---
// relationships:
//   validates: intent-driven-polyglot-release
// ---

"use strict";

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { Readable } = require("node:stream");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const installer = require("../install.js");

function temporaryDirectory() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "intentional-installer-test-"));
}

function fakeGet(responses) {
  return (_url, _options, callback) => {
    const request = new EventEmitter();
    queueMicrotask(() => {
      const next = responses.shift();
      const response = Readable.from(next.body || "");
      response.statusCode = next.statusCode;
      response.headers = next.headers || {};
      callback(response);
    });
    return request;
  };
}

test("selectTarget maps only release-matrix platforms", () => {
  assert.equal(installer.selectTarget("linux", "x64").asset, "intentional-linux-x86_64.tar.gz");
  assert.equal(installer.selectTarget("linux", "arm64").asset, "intentional-linux-arm64.tar.gz");
  assert.equal(installer.selectTarget("darwin", "arm64").asset, "intentional-macos-arm64.tar.gz");
  assert.equal(installer.selectTarget("win32", "x64").asset, "intentional-windows-x86_64.zip");
  assert.throws(() => installer.selectTarget("darwin", "x64"), /unsupported platform/);
});

test("releaseUrls binds the package version and exact asset", () => {
  const target = installer.selectTarget("linux", "x64");
  assert.deepEqual(installer.releaseUrls("1.2.3", target), {
    archive:
      "https://github.com/wyrd-company/intentional/releases/download/1.2.3/intentional-linux-x86_64.tar.gz",
    checksums: "https://github.com/wyrd-company/intentional/releases/download/1.2.3/SHA256SUMS",
  });
  assert.throws(() => installer.releaseUrls("1.2.3-beta.1", target), /plain Semantic Versioning/);
});

test("download follows bounded HTTPS redirects and rejects HTTP errors", async () => {
  const directory = temporaryDirectory();
  const destination = path.join(directory, "asset");
  await installer.download("https://example.invalid/start", destination, {
    get: fakeGet([
      { statusCode: 302, headers: { location: "/next" } },
      { statusCode: 200, body: "payload" },
    ]),
    maxRedirects: 1,
  });
  assert.equal(fs.readFileSync(destination, "utf8"), "payload");

  await assert.rejects(
    installer.download("https://example.invalid/start", path.join(directory, "overflow"), {
      get: fakeGet([
        { statusCode: 302, headers: { location: "/one" } },
        { statusCode: 302, headers: { location: "/two" } },
      ]),
      maxRedirects: 1,
    }),
    /exceeded 1 redirects/,
  );
  await assert.rejects(
    installer.download("https://example.invalid/missing", path.join(directory, "missing"), {
      get: fakeGet([{ statusCode: 404 }]),
    }),
    /HTTP 404/,
  );
  await assert.rejects(
    installer.download("http://example.invalid/insecure", path.join(directory, "insecure"), {
      get: fakeGet([]),
    }),
    /non-HTTPS/,
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("checksum evidence must be unique and must match the archive", () => {
  const directory = temporaryDirectory();
  const archive = path.join(directory, "intentional-linux-x86_64.tar.gz");
  fs.writeFileSync(archive, "verified bytes");
  const digest = crypto.createHash("sha256").update("verified bytes").digest("hex");
  const expected = installer.parseChecksum(`${digest}  intentional-linux-x86_64.tar.gz\n`, path.basename(archive));
  installer.verifyChecksum(archive, expected);
  assert.throws(
    () => installer.parseChecksum("", path.basename(archive)),
    /exactly one checksum/,
  );
  assert.throws(
    () => installer.verifyChecksum(archive, "0".repeat(64)),
    /checksum mismatch/,
  );
  fs.rmSync(directory, { recursive: true, force: true });
});

test("archive boundaries reject traversal, absolute, foreign, and duplicate executable paths", () => {
  const target = installer.selectTarget("linux", "x64");
  const valid = [
    "intentional-linux-x86_64/",
    "intentional-linux-x86_64/intentional",
    "intentional-linux-x86_64/LICENSE",
  ];
  assert.equal(
    installer.validateArchiveEntries(valid, target),
    "intentional-linux-x86_64/intentional",
  );
  for (const unsafe of [
    "../intentional",
    "/intentional-linux-x86_64/intentional",
    "C:\\intentional.exe",
    "other/intentional",
    "intentional-linux-x86_64/../intentional",
  ]) {
    assert.throws(
      () => installer.validateArchiveEntries([...valid, unsafe], target),
      /unsafe path/,
    );
  }
  assert.throws(
    () => installer.validateArchiveEntries([...valid, valid[1]], target),
    /exactly one/,
  );
});

test("extractExecutable places only the expected executable with executable permissions", () => {
  const directory = temporaryDirectory();
  const fixture = path.join(directory, "fixture", "intentional-linux-x86_64");
  fs.mkdirSync(fixture, { recursive: true });
  fs.writeFileSync(path.join(fixture, "intentional"), "native executable");
  fs.writeFileSync(path.join(fixture, "README.md"), "documentation");
  const archive = path.join(directory, "intentional-linux-x86_64.tar.gz");
  const packed = spawnSync(
    "tar",
    ["-czf", archive, "-C", path.join(directory, "fixture"), "intentional-linux-x86_64"],
    { encoding: "utf8" },
  );
  assert.equal(packed.status, 0, packed.stderr);

  const destination = path.join(directory, "output", "intentional-native");
  installer.extractExecutable(archive, installer.selectTarget("linux", "x64"), destination);
  assert.equal(fs.readFileSync(destination, "utf8"), "native executable");
  if (process.platform !== "win32") {
    assert.equal(fs.statSync(destination).mode & 0o111, 0o111);
  }
  assert.deepEqual(fs.readdirSync(path.dirname(destination)), ["intentional-native"]);
  fs.rmSync(directory, { recursive: true, force: true });
});
