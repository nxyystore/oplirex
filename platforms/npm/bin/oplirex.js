#!/usr/bin/env node
const { spawn } = require("child_process");
const { createWriteStream, existsSync, mkdirSync, chmodSync } = require("fs");
const { join } = require("path");
const { homedir, platform, arch } = require("os");
const https = require("https");

const VERSION = require("../package.json").version;
const REPO = "nxyystore/oplire";

function getAsset() {
    const p = platform();
    const a = arch();
    if (p === "win32")
        return {
            file: `oplirex-windows-x86_64.zip`,
            bin: "oplirex.exe",
            isZip: true,
        };
    if (p === "linux") {
        if (a === "arm64")
            return {
                file: `oplirex-linux-aarch64.tar.gz`,
                bin: "oplirex",
                isZip: false,
            };
        return {
            file: `oplirex-linux-x86_64.tar.gz`,
            bin: "oplirex",
            isZip: false,
        };
    }
    if (p === "darwin") {
        if (a === "arm64")
            return {
                file: `oplirex-macos-arm64.tar.gz`,
                bin: "oplirex",
                isZip: false,
            };
        return {
            file: `oplirex-macos-x86_64.tar.gz`,
            bin: "oplirex",
            isZip: false,
        };
    }
    throw new Error(`Unsupported platform ${p} ${a}`);
}

function download(url, dest) {
    return new Promise((resolve, reject) => {
        const file = createWriteStream(dest);
        https
            .get(url, { headers: { "User-Agent": "oplirex-npm" } }, (res) => {
                if (
                    res.statusCode >= 300 &&
                    res.statusCode < 400 &&
                    res.headers.location
                ) {
                    return download(res.headers.location, dest).then(
                        resolve,
                        reject,
                    );
                }
                if (res.statusCode !== 200)
                    return reject(
                        new Error(`HTTP ${res.statusCode} for ${url}`),
                    );
                res.pipe(file);
                file.on("finish", () => file.close(resolve));
            })
            .on("error", reject);
    });
}

async function ensureBinary() {
    const asset = getAsset();
    const cacheDir = join(homedir(), ".oplirex", VERSION);
    const binPath = join(cacheDir, asset.bin);
    if (existsSync(binPath)) return binPath;
    mkdirSync(cacheDir, { recursive: true });
    const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${asset.file}`;
    console.error(`[oplirex] downloading ${url} ...`);
    const tmp = join(cacheDir, asset.file);
    await download(url, tmp);
    // extract
    const { execSync } = require("child_process");
    try {
        if (asset.isZip)
            execSync(
                `powershell -Command "Expand-Archive -Path '${tmp}' -DestinationPath '${cacheDir}' -Force"`,
                { stdio: "inherit" },
            );
        else
            execSync(`tar xzf "${tmp}" -C "${cacheDir}"`, { stdio: "inherit" });
    } catch (e) {
        // fallback: if zip contained exe directly or tar failed, try direct move
    }
    // find binary if not at expected path
    if (!existsSync(binPath)) {
        const { readdirSync, statSync } = require("fs");
        const walk = (dir) => {
            for (const e of readdirSync(dir)) {
                const p = join(dir, e);
                try {
                    if (statSync(p).isDirectory()) {
                        const r = walk(p);
                        if (r) return r;
                    } else if (e === asset.bin) return p;
                } catch {}
            }
        };
        const found = walk(cacheDir);
        if (found) return found;
        throw new Error(`Binary ${asset.bin} not found after extract`);
    }
    try {
        chmodSync(binPath, 0o755);
    } catch {}
    return binPath;
}

(async () => {
    const bin = await ensureBinary();
    const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
    child.on("exit", (code) => process.exit(code ?? 0));
    child.on("error", (err) => {
        console.error(err);
        process.exit(1);
    });
})();
