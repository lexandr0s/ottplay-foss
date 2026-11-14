"use strict";
// Public dashboard files only. Never copy bridge configuration or HA credentials.
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const FILES = Object.freeze([
    "index.html",
    "style.css",
    "core.js",
    "webos-host.js",
    "app.js",
]);
const LIMIT = 1024 * 1024;
const OUTPUT_FILES = Object.freeze([...FILES, "host.js"]);
function invalid() {
    throw new Error("Unsafe Home dashboard asset or changed path");
}
function directoryChain(dir) {
    const absolute = path.resolve(dir),
        rows = [];
    let current = path.parse(absolute).root;
    for (const part of [
        "",
        ...absolute.slice(current.length).split(path.sep).filter(Boolean),
    ]) {
        if (part) current = path.join(current, part);
        const stat = fs.lstatSync(current);
        if (stat.isSymbolicLink() || !stat.isDirectory()) invalid();
        rows.push({ file: current, dev: stat.dev, ino: stat.ino });
    }
    return rows;
}
function verifyDirectories(rows) {
    for (const row of rows) {
        const stat = fs.lstatSync(row.file);
        if (
            stat.isSymbolicLink() ||
            !stat.isDirectory() ||
            stat.dev !== row.dev ||
            stat.ino !== row.ino
        )
            invalid();
    }
}
function directory(dir, create) {
    const parents = directoryChain(path.dirname(dir));
    let stat;
    try {
        stat = fs.lstatSync(dir);
    } catch (error) {
        if (error.code !== "ENOENT" || !create) throw error;
        verifyDirectories(parents);
        fs.mkdirSync(dir);
        stat = fs.lstatSync(dir);
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) invalid();
    verifyDirectories(parents);
    return directoryChain(dir);
}
function sameFile(a, b) {
    return (
        a.isFile() &&
        !a.isSymbolicLink() &&
        a.nlink === 1 &&
        b.nlink === 1 &&
        a.dev === b.dev &&
        a.ino === b.ino
    );
}
function sameVersion(a, b) {
    return (
        sameFile(a, b) &&
        a.size === b.size &&
        a.mtimeMs === b.mtimeMs &&
        a.ctimeMs === b.ctimeMs
    );
}
function readPublicFile(file, before, parents) {
    verifyDirectories(parents);
    const fd = fs.openSync(
        file,
        fs.constants.O_RDONLY |
            (fs.constants.O_NOFOLLOW || 0) |
            (fs.constants.O_NONBLOCK || 0)
    );
    try {
        const initial = fs.fstatSync(fd);
        if (!sameVersion(initial, before) || initial.size > LIMIT) invalid();
        verifyDirectories(parents);
        const bytes = Buffer.alloc(initial.size + 1);
        let count = 0,
            read;
        while (
            count < bytes.length &&
            (read = fs.readSync(fd, bytes, count, bytes.length - count, null)) >
                0
        )
            count += read;
        const after = fs.fstatSync(fd),
            leaf = fs.lstatSync(file);
        if (
            count !== initial.size ||
            !sameVersion(after, initial) ||
            !sameVersion(leaf, initial)
        )
            invalid();
        verifyDirectories(parents);
        return bytes.subarray(0, count);
    } finally {
        fs.closeSync(fd);
    }
}
function writePublicFile(file, body, before, parents) {
    verifyDirectories(parents);
    const flags =
        fs.constants.O_WRONLY |
        (fs.constants.O_NOFOLLOW || 0) |
        (fs.constants.O_NONBLOCK || 0) |
        (before ? 0 : fs.constants.O_CREAT | fs.constants.O_EXCL);
    // Never truncate a path before its opened descriptor has been admitted.
    const fd = fs.openSync(file, flags, 0o644);
    try {
        const initial = fs.fstatSync(fd),
            leaf = fs.lstatSync(file);
        if (
            !initial.isFile() ||
            initial.nlink !== 1 ||
            (before && !sameVersion(initial, before)) ||
            !sameFile(leaf, initial)
        )
            invalid();
        verifyDirectories(parents);
        fs.ftruncateSync(fd, 0);
        fs.writeFileSync(fd, body);
        const after = fs.fstatSync(fd),
            published = fs.lstatSync(file);
        if (
            !sameFile(after, initial) ||
            after.size !== body.length ||
            !sameVersion(published, after)
        )
            invalid();
        verifyDirectories(parents);
    } finally {
        fs.closeSync(fd);
    }
}
// Compile one fixed public host source at build time. It has no imports into
// the main graph and installs a factory without opening a panel on execution.
function generateHostScript(sourceRoot, compiler) {
    const file = path.join(
        sourceRoot,
        "src",
        "plugins",
        "home-integration-runtime.ts"
    );
    const parents = directory(path.dirname(file), false),
        stat = fs.lstatSync(file);
    if (
        !stat.isFile() ||
        stat.isSymbolicLink() ||
        stat.nlink !== 1 ||
        stat.size > LIMIT
    )
        invalid();
    const source = readPublicFile(file, stat, parents).toString("utf8");
    const ts =
        compiler ||
        require(require.resolve("typescript", { paths: [sourceRoot] }));
    const output = ts.transpileModule(source, {
        fileName: "home-integration-runtime.ts",
        compilerOptions: {
            target: ts.ScriptTarget.ES5,
            module: ts.ModuleKind.CommonJS,
            removeComments: true,
        },
        reportDiagnostics: true,
    });
    if (
        (output.diagnostics || []).some(
            (item) => item.category === ts.DiagnosticCategory.Error
        ) ||
        /\brequire\s*\(/.test(output.outputText)
    )
        invalid();
    const body = Buffer.from(
        "(function(window){var exports={};\n" +
            output.outputText +
            "\nwindow.__ottHomeHostInstall=exports.installHomeIntegrationRuntime;})(window);\n"
    );
    if (!body.length || body.length > LIMIT) invalid();
    return body;
}
function stageHomeIntegration(sourceRoot, targetRoot, compiler) {
    const source = path.join(sourceRoot, "integrations", "home");
    const sourceParents = directory(source, false);
    // Validate the entire fixed set before mutating the output directory.
    const inputs = FILES.map((name) => {
        verifyDirectories(sourceParents);
        const file = path.join(source, name),
            stat = fs.lstatSync(file);
        if (
            !stat.isFile() ||
            stat.isSymbolicLink() ||
            stat.nlink !== 1 ||
            stat.size > LIMIT
        )
            throw new Error("Invalid public Home dashboard asset");
        verifyDirectories(sourceParents);
        return { name, file, stat };
    });
    const payloads = inputs.map(({ name, file, stat }) => {
        const body = readPublicFile(file, stat, sourceParents);
        return {
            name,
            body,
            sha256: crypto.createHash("sha256").update(body).digest("hex"),
        };
    });
    const host = generateHostScript(sourceRoot, compiler);
    payloads.push({
        name: "host.js",
        body: host,
        sha256: crypto.createHash("sha256").update(host).digest("hex"),
    });
    directory(targetRoot, true);
    directory(path.join(targetRoot, "dist"), true);
    // Server serves repo/dist/home; hosted/native web roots serve /dist/home.
    // Both copies come from the same validated public payloads, never a directory copy.
    const outputs = [
        path.join(targetRoot, "home"),
        path.join(targetRoot, "dist", "home"),
    ];
    const destinations = outputs.map((output) => {
        const parents = directory(output, true),
            expected = new Map();
        for (const file of [...OUTPUT_FILES, "assets.json"]) {
            verifyDirectories(parents);
            const target = path.join(output, file);
            let stat;
            try {
                stat = fs.lstatSync(target);
            } catch (error) {
                if (error.code !== "ENOENT") throw error;
            }
            if (
                stat &&
                (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1)
            )
                throw new Error("Unsafe Home dashboard output");
            expected.set(file, stat || null);
        }
        for (const file of fs.readdirSync(output)) {
            if (![...OUTPUT_FILES, "assets.json"].includes(file))
                throw new Error(
                    "Unexpected Home dashboard output; clean build tree required"
                );
        }
        verifyDirectories(parents);
        return { output, parents, expected };
    });
    const manifest = {
        version: 1,
        files: payloads.map(({ name, sha256, body }) => ({
            name,
            sha256,
            bytes: body.length,
        })),
    };
    for (const { output, parents, expected } of destinations) {
        for (const item of payloads)
            writePublicFile(
                path.join(output, item.name),
                item.body,
                expected.get(item.name),
                parents
            );
        writePublicFile(
            path.join(output, "assets.json"),
            Buffer.from(JSON.stringify(manifest, null, 2) + "\n"),
            expected.get("assets.json"),
            parents
        );
    }
    return manifest;
}
// Copy an already-built public payload without loading a TypeScript compiler.
// Read and validate the complete fixed inventory before creating any destination.
function readBuiltHomeIntegration(source) {
    const parents = directory(source, false);
    const names = [...OUTPUT_FILES, "assets.json"];
    if (
        JSON.stringify(fs.readdirSync(source).sort()) !==
        JSON.stringify([...names].sort())
    )
        invalid();
    const payloads = new Map(
        names.map((name) => {
            const file = path.join(source, name),
                stat = fs.lstatSync(file);
            if (
                !stat.isFile() ||
                stat.isSymbolicLink() ||
                stat.nlink !== 1 ||
                !stat.size ||
                stat.size > LIMIT
            )
                invalid();
            return [name, readPublicFile(file, stat, parents)];
        })
    );
    const manifest = JSON.parse(payloads.get("assets.json").toString("utf8"));
    if (
        Object.keys(manifest).sort().join(",") !== "files,version" ||
        manifest.version !== 1 ||
        !Array.isArray(manifest.files) ||
        manifest.files.length !== OUTPUT_FILES.length
    )
        invalid();
    const seen = new Set();
    for (const item of manifest.files) {
        if (
            !item ||
            Object.keys(item).sort().join(",") !== "bytes,name,sha256" ||
            !OUTPUT_FILES.includes(item.name) ||
            seen.has(item.name)
        )
            invalid();
        seen.add(item.name);
        const body = payloads.get(item.name);
        if (
            item.bytes !== body.length ||
            item.sha256 !==
                crypto.createHash("sha256").update(body).digest("hex")
        )
            invalid();
    }
    return { manifest, payloads };
}
function validateBuiltHomeIntegration(source) {
    return readBuiltHomeIntegration(source).manifest;
}
function copyBuiltHomeIntegration(source, destination) {
    const { manifest, payloads } = readBuiltHomeIntegration(source);
    const outputParents = directory(destination, true);
    if (fs.readdirSync(destination).length) invalid();
    for (const [name, body] of payloads)
        writePublicFile(
            path.join(destination, name),
            body,
            null,
            outputParents
        );
    return manifest;
}
module.exports = {
    FILES,
    OUTPUT_FILES,
    copyBuiltHomeIntegration,
    generateHostScript,
    stageHomeIntegration,
    validateBuiltHomeIntegration,
};
