#!/usr/bin/env node
// Runs the atelier binary for this machine, so an MCP client can start
// Atelier with `npx -y @marmikshah/atelier` and nothing else installed.
//
// The first run downloads the release archive whose version matches this
// package, checks it against the release's SHA256SUMS, and caches the binary;
// later runs start the cached copy straight away. Arguments, stdin, stdout,
// and the exit status pass through unchanged, and anything this wrapper says
// goes to stderr, so stdout stays a clean MCP stream.
//
// Environment:
//   ATELIER_CACHE_DIR     where binaries are cached (default: ~/Library/Caches/atelier
//                         on macOS, $XDG_CACHE_HOME/atelier or ~/.cache/atelier on Linux)
//   ATELIER_DOWNLOAD_URL  base URL that holds the release files, for mirrors and tests

"use strict";

const { execFileSync, spawn } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { version } = require("../package.json");

const TARGETS = {
    "darwin arm64": "aarch64-apple-darwin",
    "darwin x64": "x86_64-apple-darwin",
    "linux arm64": "aarch64-unknown-linux-musl",
    "linux x64": "x86_64-unknown-linux-musl",
};

function cacheRoot() {
    if (process.env.ATELIER_CACHE_DIR) {
        return process.env.ATELIER_CACHE_DIR;
    }
    if (process.platform === "darwin") {
        return path.join(os.homedir(), "Library", "Caches", "atelier");
    }
    const base = process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
    return path.join(base, "atelier");
}

async function download(url) {
    const response = await fetch(url);
    if (!response.ok) {
        throw new Error(`could not download ${url} (HTTP ${response.status})`);
    }
    return Buffer.from(await response.arrayBuffer());
}

/** The cached binary for this machine, downloading it first if needed. */
async function binary() {
    const target = TARGETS[`${process.platform} ${process.arch}`];
    if (!target) {
        throw new Error(
            `there is no prebuilt atelier for ${process.platform} ${process.arch}; ` +
                "see https://github.com/marmikshah/atelier#build-from-source",
        );
    }
    const dir = path.join(cacheRoot(), version, target);
    const cached = path.join(dir, "atelier");
    if (fs.existsSync(cached)) {
        return cached;
    }

    const base = (
        process.env.ATELIER_DOWNLOAD_URL ||
        `https://github.com/marmikshah/atelier/releases/download/v${version}`
    ).replace(/\/$/, "");
    const asset = `atelier-${target}.tar.gz`;
    process.stderr.write(`atelier: downloading ${asset} (v${version})\n`);
    const [archive, sums] = await Promise.all([
        download(`${base}/${asset}`),
        download(`${base}/SHA256SUMS`),
    ]);
    const expected = sums
        .toString("utf8")
        .split("\n")
        .map((line) => line.trim().split(/\s+/))
        .find(([, file]) => file === asset || file === `*${asset}`)?.[0];
    if (!expected) {
        throw new Error(`SHA256SUMS does not list ${asset}`);
    }
    const actual = crypto.createHash("sha256").update(archive).digest("hex");
    if (actual !== expected) {
        throw new Error(`${asset} does not match its checksum`);
    }

    // Unpack beside the cache entry and rename it into place, so two clients
    // starting at once never run a half-written file.
    fs.mkdirSync(dir, { recursive: true });
    const staging = fs.mkdtempSync(path.join(dir, ".download-"));
    try {
        fs.writeFileSync(path.join(staging, asset), archive);
        execFileSync("tar", ["-xzf", asset, "atelier"], {
            cwd: staging,
            stdio: ["ignore", "ignore", "inherit"],
        });
        fs.chmodSync(path.join(staging, "atelier"), 0o755);
        fs.renameSync(path.join(staging, "atelier"), cached);
    } finally {
        fs.rmSync(staging, { recursive: true, force: true });
    }
    return cached;
}

function run(file) {
    const child = spawn(file, process.argv.slice(2), { stdio: "inherit" });
    const signals = ["SIGINT", "SIGTERM", "SIGHUP"];
    for (const signal of signals) {
        process.on(signal, () => child.kill(signal));
    }
    child.on("error", (error) => {
        process.stderr.write(`atelier: cannot start ${file}: ${error.message}\n`);
        process.exit(1);
    });
    child.on("exit", (code, signal) => {
        if (signal) {
            // Die the same way, so the caller sees what the child saw.
            for (const forwarded of signals) {
                process.removeAllListeners(forwarded);
            }
            process.kill(process.pid, signal);
            return;
        }
        process.exit(code ?? 1);
    });
}

binary().then(run, (error) => {
    process.stderr.write(`atelier: ${error.message}\n`);
    process.exit(1);
});
