// End-to-end contract for the npx wrapper against a local copy of a release.
//
//   ATELIER_TEST_RELEASE=DIR node --test npm/test/
//
// DIR holds this machine's atelier-<target>.tar.gz and a SHA256SUMS that
// lists it, as a GitHub Release does. Without it the tests are skipped.

"use strict";

const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { version } = require("../package.json");

const release = process.env.ATELIER_TEST_RELEASE;
const wrapper = path.join(__dirname, "..", "bin", "atelier.js");
const skip = release ? false : "set ATELIER_TEST_RELEASE to a directory of release files";

/** Serve `dir` over HTTP on loopback, counting requests. */
async function serve(dir) {
    const server = http.createServer((request, response) => {
        server.requests += 1;
        const file = path.join(dir, path.basename(request.url));
        fs.readFile(file, (error, body) => {
            response.writeHead(error ? 404 : 200);
            response.end(error ? "" : body);
        });
    });
    server.requests = 0;
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    server.base = `http://127.0.0.1:${server.address().port}`;
    return server;
}

function tempDir(label) {
    return fs.mkdtempSync(path.join(os.tmpdir(), `atelier-npm-${label}-`));
}

/** Run the wrapper; resolves with its exit code and output. */
function wrap(args, { base, cache, input }) {
    return new Promise((resolve) => {
        const child = spawn(process.execPath, [wrapper, ...args], {
            env: {
                ...process.env,
                ATELIER_CACHE_DIR: cache,
                ATELIER_DOWNLOAD_URL: base,
                ATELIER_HOME: path.join(cache, "store"),
                ATELIER_LOG: "off",
            },
        });
        let stdout = "";
        let stderr = "";
        child.stdout.on("data", (chunk) => (stdout += chunk));
        child.stderr.on("data", (chunk) => (stderr += chunk));
        child.on("exit", (code) => resolve({ code, stdout, stderr }));
        child.stdin.end(input ?? "");
    });
}

test("downloads, checks, and caches the binary, then reuses it", { skip }, async () => {
    const server = await serve(release);
    const cache = tempDir("cache");
    try {
        const first = await wrap(["--version"], { base: server.base, cache });
        assert.equal(first.code, 0, first.stderr);
        assert.equal(first.stdout.trim(), `atelier ${version}`);
        assert.match(first.stderr, /downloading atelier-/);
        const downloads = server.requests;

        const second = await wrap(["--version"], { base: server.base, cache });
        assert.equal(second.code, 0, second.stderr);
        assert.equal(second.stderr, "", "a cached binary starts silently");
        assert.equal(server.requests, downloads, "nothing is downloaded twice");

        const bad = await wrap(["call", "no_such_tool", "{}"], { base: server.base, cache });
        assert.equal(bad.code, 2, "the binary's exit status passes through");
    } finally {
        server.close();
        fs.rmSync(cache, { recursive: true, force: true });
    }
});

test("serves MCP over stdio with a clean stdout on first run", { skip }, async () => {
    const server = await serve(release);
    const cache = tempDir("mcp");
    try {
        const initialize = JSON.stringify({
            jsonrpc: "2.0",
            id: 1,
            method: "initialize",
            params: {
                protocolVersion: "2025-06-18",
                capabilities: {},
                clientInfo: { name: "wrapper-test", version: "0" },
            },
        });
        const result = await wrap([], { base: server.base, cache, input: `${initialize}\n` });
        assert.match(result.stderr, /downloading/, "the download happened in this run");
        const reply = JSON.parse(result.stdout.split("\n")[0]);
        assert.equal(reply.id, 1);
        assert.ok(reply.result.serverInfo, result.stdout);
    } finally {
        server.close();
        fs.rmSync(cache, { recursive: true, force: true });
    }
});

test("refuses an archive that does not match its checksum", { skip }, async () => {
    const tampered = tempDir("tampered");
    const cache = tempDir("refuse");
    for (const file of fs.readdirSync(release)) {
        fs.copyFileSync(path.join(release, file), path.join(tampered, file));
    }
    const archive = fs.readdirSync(tampered).find((file) => file.endsWith(".tar.gz"));
    fs.appendFileSync(path.join(tampered, archive), "junk");
    const server = await serve(tampered);
    try {
        const result = await wrap(["--version"], { base: server.base, cache });
        assert.equal(result.code, 1);
        assert.match(result.stderr, /does not match its checksum/);
        assert.equal(result.stdout, "");
        assert.equal(fs.readdirSync(cache).length, 0, "nothing is cached");
    } finally {
        server.close();
        fs.rmSync(tampered, { recursive: true, force: true });
        fs.rmSync(cache, { recursive: true, force: true });
    }
});
