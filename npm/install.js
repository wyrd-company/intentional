#!/usr/bin/env node
// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { pipeline } = require("node:stream/promises");
const { spawnSync } = require("node:child_process");

const REPOSITORY = "wyrd-company/intentional";
const MAX_REDIRECTS = 5;
const TARGETS = Object.freeze({
  "linux/x64": {
    asset: "intentional-linux-x86_64.tar.gz",
    directory: "intentional-linux-x86_64",
    executable: "intentional",
  },
  "linux/arm64": {
    asset: "intentional-linux-arm64.tar.gz",
    directory: "intentional-linux-arm64",
    executable: "intentional",
  },
  "darwin/arm64": {
    asset: "intentional-macos-arm64.tar.gz",
    directory: "intentional-macos-arm64",
    executable: "intentional",
  },
  "win32/x64": {
    asset: "intentional-windows-x86_64.zip",
    directory: "intentional-windows-x86_64",
    executable: "intentional.exe",
  },
});

function selectTarget(platform = process.platform, architecture = process.arch) {
  const key = `${platform}/${architecture}`;
  const target = TARGETS[key];
  if (!target) {
    throw new Error(
      `unsupported platform ${key}; install intentional-cli with Cargo or build Intentional from source`,
    );
  }
  return target;
}

function releaseUrls(version, target) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error(`package version ${version} is not plain Semantic Versioning`);
  }
  const base = `https://github.com/${REPOSITORY}/releases/download/${version}`;
  return {
    archive: `${base}/${target.asset}`,
    checksums: `${base}/SHA256SUMS`,
  };
}

function requestResponse(url, get = https.get) {
  return new Promise((resolve, reject) => {
    const request = get(
      url,
      { headers: { "User-Agent": "intentional-npm-installer" } },
      resolve,
    );
    request.once("error", reject);
  });
}

async function download(url, destination, options = {}) {
  const get = options.get || https.get;
  const maxRedirects = options.maxRedirects ?? MAX_REDIRECTS;
  let current = new URL(url);

  for (let redirects = 0; ; redirects += 1) {
    if (current.protocol !== "https:") {
      throw new Error(`refusing non-HTTPS download URL ${current}`);
    }

    const response = await requestResponse(current, get);
    const location = response.headers.location;
    if (response.statusCode >= 300 && response.statusCode < 400 && location) {
      response.resume();
      if (redirects >= maxRedirects) {
        throw new Error(`download exceeded ${maxRedirects} redirects`);
      }
      current = new URL(location, current);
      continue;
    }
    if (response.statusCode !== 200) {
      response.resume();
      throw new Error(`download failed with HTTP ${response.statusCode} for ${current}`);
    }

    const temporary = `${destination}.partial`;
    try {
      await pipeline(response, fs.createWriteStream(temporary, { flags: "wx" }));
      fs.renameSync(temporary, destination);
      return;
    } catch (error) {
      fs.rmSync(temporary, { force: true });
      throw error;
    }
  }
}

function parseChecksum(contents, asset) {
  const matches = [];
  for (const line of contents.split(/\r?\n/)) {
    const match = /^([a-fA-F0-9]{64})\s+\*?(.+)$/.exec(line.trim());
    if (match && match[2] === asset) {
      matches.push(match[1].toLowerCase());
    }
  }
  if (matches.length !== 1) {
    throw new Error(`SHA256SUMS must contain exactly one checksum for ${asset}`);
  }
  return matches[0];
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function verifyChecksum(file, expected) {
  const actual = sha256(file);
  if (!crypto.timingSafeEqual(Buffer.from(actual), Buffer.from(expected))) {
    throw new Error(`checksum mismatch for ${path.basename(file)}`);
  }
}

function runTar(args, options = {}) {
  const encoding = Object.hasOwn(options, "encoding") ? options.encoding : "utf8";
  const result = (options.spawnSync || spawnSync)("tar", args, {
    encoding,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) {
    throw new Error(`failed to run tar: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = Buffer.isBuffer(result.stderr)
      ? result.stderr.toString("utf8").trim()
      : String(result.stderr || "").trim();
    throw new Error(`tar failed${detail ? `: ${detail}` : ""}`);
  }
  return result.stdout;
}

function listArchive(archive, target, options = {}) {
  const compressed = target.asset.endsWith(".tar.gz");
  const output = runTar([compressed ? "-tzf" : "-tf", archive], options);
  return String(output)
    .split(/\r?\n/)
    .filter(Boolean);
}

function validateArchiveEntries(entries, target) {
  const expected = `${target.directory}/${target.executable}`;
  let executableCount = 0;

  for (const entry of entries) {
    const normalized = entry.replaceAll("\\", "/").replace(/\/$/, "");
    const parts = normalized.split("/");
    if (
      !normalized ||
      normalized.startsWith("/") ||
      /^[A-Za-z]:/.test(normalized) ||
      parts.includes("..") ||
      parts.includes(".") ||
      (normalized !== target.directory && !normalized.startsWith(`${target.directory}/`))
    ) {
      throw new Error(`archive contains an unsafe path: ${entry}`);
    }
    if (normalized === expected) {
      executableCount += 1;
    }
  }

  if (executableCount !== 1) {
    throw new Error(`archive must contain exactly one ${expected}`);
  }
  return expected;
}

function extractExecutable(archive, target, destination, options = {}) {
  const entry = validateArchiveEntries(listArchive(archive, target, options), target);
  const compressed = target.asset.endsWith(".tar.gz");
  const contents = runTar(
    [compressed ? "-xOzf" : "-xOf", archive, entry],
    { ...options, encoding: null },
  );
  if (!Buffer.isBuffer(contents) || contents.length === 0) {
    throw new Error(`archive member ${entry} is empty`);
  }

  fs.mkdirSync(path.dirname(destination), { recursive: true });
  const temporary = `${destination}.partial`;
  try {
    fs.writeFileSync(temporary, contents, { flag: "wx", mode: 0o755 });
    if (process.platform !== "win32") {
      fs.chmodSync(temporary, 0o755);
    }
    fs.rmSync(destination, { force: true });
    fs.renameSync(temporary, destination);
  } catch (error) {
    fs.rmSync(temporary, { force: true });
    throw error;
  }
}

async function install(options = {}) {
  const packageRoot = options.packageRoot || __dirname;
  const packageMetadata = options.packageMetadata || require("./package.json");
  const target = options.target || selectTarget();
  const urls = releaseUrls(packageMetadata.version, target);
  const temporaryDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "intentional-install-"));
  const checksumFile = path.join(temporaryDirectory, "SHA256SUMS");
  const archive = path.join(temporaryDirectory, target.asset);
  const nativeName = process.platform === "win32" ? "intentional-native.exe" : "intentional-native";
  const destination = path.join(packageRoot, "bin", nativeName);

  try {
    console.log(`intentional: downloading ${target.asset}`);
    await (options.download || download)(urls.checksums, checksumFile);
    await (options.download || download)(urls.archive, archive);
    const expected = parseChecksum(fs.readFileSync(checksumFile, "utf8"), target.asset);
    verifyChecksum(archive, expected);
    (options.extractExecutable || extractExecutable)(archive, target, destination);
    if (!fs.existsSync(destination)) {
      throw new Error("installer completed without an intentional executable");
    }
    console.log("intentional: installed checksum-verified native executable");
    return destination;
  } finally {
    fs.rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}

if (require.main === module) {
  install().catch((error) => {
    console.error(`intentional: install failed: ${error.message}`);
    process.exitCode = 1;
  });
}

module.exports = {
  MAX_REDIRECTS,
  TARGETS,
  download,
  extractExecutable,
  install,
  listArchive,
  parseChecksum,
  releaseUrls,
  selectTarget,
  validateArchiveEntries,
  verifyChecksum,
};
